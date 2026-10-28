//! Stack-overflow detection on the stack pointer itself.
//!
//! ASSIST_DEBUG's SP monitor raises an interrupt the moment SP leaves
//! `[min, max]`, however large the frame that moved it. The guard-word
//! watchpoint catches only a store to that one word, so a frame larger than the
//! gap below it jumps the guard and the overflow runs on unseen.
//!
//! `min` sits [`MARGIN`] bytes above the stack's bottom, so the trap that
//! reports an overflow still has room to push its frame. The handler then
//! moves to an emergency stack of its own before it panics, so the report never
//! runs on the stack that overflowed.
//!
//! A scheduler switching stacks stages the next stack's bounds with [`stage`];
//! its context-restore code applies them around the SP load, reading the
//! per-core [`Monitor`] block (esp-rtos does this in its switch trampoline).

use core::ptr::addr_of_mut;

use crate::{
    interrupt::{InterruptHandler, Priority},
    peripherals::{ASSIST_DEBUG, Interrupt},
    system::Cpu,
};

/// Bytes between a stack's bottom and the monitor's lower bound.
pub const MARGIN: usize = esp_config::esp_config_int!(usize, "ESP_HAL_CONFIG_STACK_MONITOR_MARGIN");

const EMERGENCY_STACK: usize = 4096;

/// One core's monitor, laid out for a context switch written in assembly.
///
/// Offsets are part of the contract with that assembly: [`Monitor::ARMED`]
/// and the other `const`s name them, and the struct is `repr(C)` and 64 bytes.
#[repr(C, align(64))]
pub struct Monitor {
    /// Non-zero once the monitor guards this core.
    pub armed: u32,
    /// `MONTR_ENA`, and its value with the SP monitor stopped and running.
    pub montr_ena: *mut u32,
    pub montr_stopped: u32,
    pub montr_running: u32,
    pub sp_min: *mut u32,
    pub sp_max: *mut u32,
    /// `INTR_CLR`, written with [`Monitor::clear_bits`] to drop a latch.
    pub intr_clr: *mut u32,
    pub clear_bits: u32,
    /// Bounds the next context restore applies.
    pub next_min: u32,
    pub next_max: u32,
}

impl Monitor {
    pub const ARMED: usize = 0;
    pub const MONTR_ENA: usize = 4;
    pub const MONTR_STOPPED: usize = 8;
    pub const MONTR_RUNNING: usize = 12;
    pub const SP_MIN: usize = 16;
    pub const SP_MAX: usize = 20;
    pub const INTR_CLR: usize = 24;
    pub const CLEAR_BITS: usize = 28;
    pub const NEXT_MIN: usize = 32;
    pub const NEXT_MAX: usize = 36;
    /// `log2` of the stride between cores' blocks.
    pub const STRIDE_SHIFT: usize = 6;

    const fn new() -> Self {
        Self {
            armed: 0,
            montr_ena: core::ptr::null_mut(),
            montr_stopped: 0,
            montr_running: 0,
            sp_min: core::ptr::null_mut(),
            sp_max: core::ptr::null_mut(),
            intr_clr: core::ptr::null_mut(),
            clear_bits: 0,
            next_min: 0,
            next_max: 0,
        }
    }
}

const _: () = assert!(core::mem::size_of::<Monitor>() == 1 << Monitor::STRIDE_SHIFT);
const _: () = assert!(core::mem::offset_of!(Monitor, next_max) == Monitor::NEXT_MAX);

/// Every core's [`Monitor`], indexed by `mhartid`.
#[unsafe(no_mangle)]
pub static mut ESP_HAL_STACK_MONITOR: [Monitor; Cpu::COUNT] =
    [const { Monitor::new() }; Cpu::COUNT];

#[repr(C, align(16))]
struct EmergencyStack([u8; EMERGENCY_STACK]);

static mut EMERGENCY_STACKS: [EmergencyStack; Cpu::COUNT] =
    [const { EmergencyStack([0; EMERGENCY_STACK]) }; Cpu::COUNT];

fn monitor(cpu: Cpu) -> *mut Monitor {
    // SAFETY: an in-bounds element of a static; callers serialise per core.
    unsafe { addr_of_mut!(ESP_HAL_STACK_MONITOR[cpu as usize]) }
}

/// The monitor's bounds for a stack spanning `bottom..top`.
pub const fn bounds(bottom: usize, top: usize) -> (u32, u32) {
    ((bottom + MARGIN) as u32, top as u32)
}

/// Guard the current core's stack, `bottom..top`, which the caller is running
/// on, and take the monitor's interrupt on this core.
///
/// # Safety
/// The caller's SP must lie inside the bounds this sets.
pub unsafe fn arm_current(bottom: usize, top: usize) {
    let cpu = Cpu::current();
    let regs = ASSIST_DEBUG::regs().cpu(cpu as usize);
    let m = monitor(cpu);
    let (min, max) = bounds(bottom, top);
    // Every mask from the PAC's own fields: the bits differ between chips
    // (0 and 1 on the C6, 8 and 9 on the S31).
    let running = regs
        .montr_ena()
        .modify(|_, w| w.sp_spill_min_ena().set_bit().sp_spill_max_ena().set_bit());
    let stopped = regs.montr_ena().modify(|_, w| {
        w.sp_spill_min_ena()
            .clear_bit()
            .sp_spill_max_ena()
            .clear_bit()
    });
    regs.sp_min().write(|w| unsafe { w.sp_min().bits(min) });
    regs.sp_max().write(|w| unsafe { w.sp_max().bits(max) });
    let clear_bits = regs
        .intr_clr()
        .write(|w| w.sp_spill_min_clr().set_bit().sp_spill_max_clr().set_bit());
    regs.intr_ena().modify(|_, w| {
        w.sp_spill_min_intr_ena()
            .set_bit()
            .sp_spill_max_intr_ena()
            .set_bit()
    });
    unsafe {
        (*m).montr_ena = regs.montr_ena().as_ptr();
        (*m).montr_stopped = stopped;
        (*m).montr_running = running;
        (*m).sp_min = regs.sp_min().as_ptr();
        (*m).sp_max = regs.sp_max().as_ptr();
        (*m).intr_clr = regs.intr_clr().as_ptr();
        (*m).clear_bits = clear_bits;
        (*m).next_min = min;
        (*m).next_max = max;
        (*m).montr_ena.write_volatile(running);
        (*m).armed = 1;
    }
    let handler = InterruptHandler::new(on_spill, Priority::max());
    if cpu == Cpu::ProCpu {
        crate::interrupt::bind_handler(Interrupt::ASSIST_DEBUG, handler);
    } else {
        crate::interrupt::enable_on_cpu(cpu, Interrupt::ASSIST_DEBUG, Priority::max());
    }
}

/// Stage `bottom..top` as the stack the current core's next context restore
/// switches to. A no-op until [`arm_current`] has run on this core.
pub fn stage(bottom: usize, top: usize) {
    let (min, max) = bounds(bottom, top);
    let m = monitor(Cpu::current());
    // SAFETY: only this core touches its own block, and the switch reads it
    // with interrupts masked after the scheduler has written it.
    unsafe {
        (*m).next_min = min;
        (*m).next_max = max;
    }
}

#[crate::ram]
extern "C" fn on_spill() {
    let cpu = Cpu::current();
    let regs = ASSIST_DEBUG::regs().cpu(cpu as usize);
    let raw = regs.intr_raw().read();
    // One interrupt source serves every core's monitor, and each core takes it.
    // A spill on another core is that core's to report: reporting it here would
    // name the wrong core and stack, and two cores panicking at once is not loud.
    if !raw.sp_spill_min_raw().bit() && !raw.sp_spill_max_raw().bit() {
        return;
    }
    let m = monitor(cpu);
    let pc = regs.sp_pc().read().bits();
    let min = regs.sp_min().read().bits();
    let max = regs.sp_max().read().bits();
    let below = raw.sp_spill_min_raw().bit();
    // Stop first: the report runs below the bound the interrupt fired on. Then
    // drop the latch, or the shared line stays raised and holds every other
    // core in this handler for as long as the report takes.
    unsafe {
        (*m).montr_ena.write_volatile((*m).montr_stopped);
        (*m).armed = 0;
        (*m).intr_clr.write_volatile((*m).clear_bits);
    }
    let sp: usize;
    unsafe { core::arch::asm!("mv {0}, sp", out(reg) sp) };
    // SAFETY: this core's emergency stack, used once: `overflowed` never returns.
    let top = unsafe { addr_of_mut!(EMERGENCY_STACKS[cpu as usize]).add(1) } as usize;
    unsafe {
        core::arch::asm!(
            "mv sp, {top}",
            "jr {f}",
            top = in(reg) top,
            f = in(reg) overflowed as *const () as usize,
            in("a0") pc,
            in("a1") sp,
            in("a2") min,
            in("a3") max,
            in("a4") u32::from(below),
            options(noreturn),
        );
    }
}

extern "C" fn overflowed(pc: u32, sp: usize, min: u32, max: u32, below: u32) -> ! {
    panic!(
        "[{:?}] Stack overflow: SP {} its stack's bounds [0x{:08x}, 0x{:08x}] at PC 0x{:08x} (handler SP 0x{:08x})",
        Cpu::current(),
        if below != 0 {
            "fell below"
        } else {
            "rose above"
        },
        min,
        max,
        pc,
        sp,
    );
}
