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
//! per-core [`Monitor`] block (esp-rtos does this in its switch trampoline on
//! RISC-V; on Xtensa, xtensa-lx-rt's level-1 exit does it for every switch).
//!
//! On Xtensa the monitor also trips without an overflow, so a trip is only a
//! trigger: the handler compares the SP in the trap frame with the bounds,
//! panics when it is out of them and otherwise counts the trip in
//! [`spurious`]. Measured on an ESP32-S3 (CoreS3, UI SECC with Wi-Fi):
//! - trips at an `entry` whose SP sample lies outside internal DRAM entirely, whatever the bounds:
//!   60-250 a second on the PRO core (the more the busier), a few on APP;
//! - trips after a flash cache miss stalls a window change past `SP_UNSTABLE` (reset value 11):
//!   every call up to 192 cycles, none from 200 at 80 MHz QIO. Arming sets the field's maximum,
//!   which only lowers the rate.
//!
//! An overflow that happens and unwinds entirely while the interrupt is masked
//! is therefore reported as spurious, as an unreported one would be anywhere.

use core::ptr::addr_of_mut;

use crate::{
    interrupt::{InterruptHandler, Priority},
    peripherals::{ASSIST_DEBUG, Interrupt},
    system::Cpu,
};

/// Bytes between a stack's bottom and the monitor's lower bound.
pub const MARGIN: usize = esp_config::esp_config_int!(usize, "ESP_HAL_CONFIG_STACK_MONITOR_MARGIN");

const EMERGENCY_STACK: usize = 4096;

/// `SP_UNSTABLE`: the most cycles after a window change the check may skip,
/// above the measured cache-miss stall (see the module docs).
#[cfg(xtensa)]
const UNSTABLE_CYCLES: u8 = u8::MAX;

#[cfg(xtensa)]
static SPURIOUS: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

/// Trips whose interrupted SP was inside its bounds, which the module docs
/// measure: a rate to report, never a reason to panic.
#[cfg(xtensa)]
pub fn spurious() -> u32 {
    SPURIOUS.load(portable_atomic::Ordering::Relaxed)
}

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

// On Xtensa the layout is xtensa-lx-rt's: its level-1 exit reads the block.
#[cfg(xtensa)]
const _: () = {
    use xtensa_lx_rt::stack_monitor as rt;
    assert!(Monitor::ARMED == rt::ARMED);
    assert!(Monitor::MONTR_ENA == rt::MONTR_ENA);
    assert!(Monitor::MONTR_STOPPED == rt::MONTR_STOPPED);
    assert!(Monitor::MONTR_RUNNING == rt::MONTR_RUNNING);
    assert!(Monitor::SP_MIN == rt::SP_MIN);
    assert!(Monitor::SP_MAX == rt::SP_MAX);
    assert!(Monitor::INTR_CLR == rt::INTR_CLR);
    assert!(Monitor::CLEAR_BITS == rt::CLEAR_BITS);
    assert!(Monitor::NEXT_MIN == rt::NEXT_MIN);
    assert!(Monitor::NEXT_MAX == rt::NEXT_MAX);
    assert!(Monitor::STRIDE_SHIFT == rt::STRIDE_SHIFT);
};

/// Every core's [`Monitor`], indexed by `mhartid` (by `PRID` bit 13 on
/// Xtensa, where xtensa-lx-rt's level-1 exit finds it by name).
#[cfg_attr(riscv, unsafe(no_mangle))]
#[cfg_attr(xtensa, unsafe(export_name = "__xtensa_lx_rt_stack_monitor"))]
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
    #[cfg(xtensa)]
    regs.sp_unstable()
        .write(|w| unsafe { w.sp_unstable().bits(UNSTABLE_CYCLES) });
    let clear_bits = regs
        .intr_clr()
        .write(|w| w.sp_spill_min_clr().set_bit().sp_spill_max_clr().set_bit());
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
    }
    // The first window change after enabling latches a stale SP once
    // (measured on the S3); let one happen, then drop it.
    prime();
    unsafe { (*m).intr_clr.write_volatile(clear_bits) };
    regs.intr_ena().modify(|_, w| {
        w.sp_spill_min_intr_ena()
            .set_bit()
            .sp_spill_max_intr_ena()
            .set_bit()
    });
    unsafe { (*m).armed = 1 };
    let handler = InterruptHandler::new(on_spill_entry(), Priority::max());
    if cpu == Cpu::ProCpu {
        crate::interrupt::bind_handler(Interrupt::ASSIST_DEBUG, handler);
    } else {
        crate::interrupt::enable_on_cpu(cpu, Interrupt::ASSIST_DEBUG, Priority::max());
    }
}

/// Stop every core's monitor and drop its latch, for a reset in progress.
///
/// Resetting a core moves its SP out of the bounds it was guarded with, and
/// the interrupt that raises is shared: on the core requesting the reset it
/// would preempt the reset itself.
pub fn disarm_all() {
    for cpu in Cpu::all() {
        let m = monitor(cpu);
        // SAFETY: the blocks of armed cores were filled by `arm_current`; a
        // core's own context switch reads `armed` before touching the rest.
        unsafe {
            if (*m).armed != 0 {
                (*m).armed = 0;
                (*m).montr_ena.write_volatile((*m).montr_stopped);
                (*m).intr_clr.write_volatile((*m).clear_bits);
            }
        }
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

/// A call, so a window change, made only for its side effect on the monitor.
#[inline(never)]
fn prime() {
    core::hint::black_box(());
}

#[cfg(riscv)]
fn on_spill_entry() -> extern "C" fn() {
    on_spill
}

/// Xtensa handlers receive the trap frame; [`InterruptHandler`] stores them as
/// `extern "C" fn()` and esp-hal's dispatch passes the frame regardless.
#[cfg(xtensa)]
fn on_spill_entry() -> extern "C" fn() {
    // SAFETY: the dispatch calls every handler as `fn(&mut Context)`.
    unsafe { core::mem::transmute::<*const (), extern "C" fn()>(on_spill as *const ()) }
}

#[cfg(riscv)]
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

#[cfg(xtensa)]
#[crate::ram]
fn on_spill(frame: &mut xtensa_lx_rt::exception::Context) {
    let cpu = Cpu::current();
    let regs = ASSIST_DEBUG::regs().cpu(cpu as usize);
    let raw = regs.intr_raw().read();
    // Shared by both cores, as on RISC-V: another core's spill is its own.
    if !raw.sp_spill_min_raw().bit() && !raw.sp_spill_max_raw().bit() {
        return;
    }
    let m = monitor(cpu);
    let min = regs.sp_min().read().bits();
    let max = regs.sp_max().read().bits();
    let sp = frame.A1;
    if (min..=max).contains(&sp) {
        // SAFETY: this core's block, filled by `arm_current`.
        unsafe { (*m).intr_clr.write_volatile((*m).clear_bits) };
        SPURIOUS.fetch_add(1, portable_atomic::Ordering::Relaxed);
        return;
    }
    let pc = regs.sp_pc().read().bits();
    unsafe {
        (*m).montr_ena.write_volatile((*m).montr_stopped);
        (*m).armed = 0;
        (*m).intr_clr.write_volatile((*m).clear_bits);
    }
    // SAFETY: this core's emergency stack, used once: `overflowed` never returns.
    let top = unsafe { addr_of_mut!(EMERGENCY_STACKS[cpu as usize]).add(1) } as usize;
    // Forget every window but this one, so no frame of the overflowed stack is
    // ever spilled, then call the report on the emergency stack. `call4` hands
    // a6..a10 over as its a2..a6. a11 is scratch: nothing returns here. a7 is
    // the frame pointer, no operand, so it is loaded last, on the new stack.
    unsafe {
        core::arch::asm!(
            "rsr.windowbase a11",
            "ssl a11",
            "movi a11, 1",
            "sll a11, a11",
            "wsr.windowstart a11",
            "rsync",
            "mov a1, {top}",
            "mov a7, {sp}",
            "callx4 {f}",
            top = in(reg) top,
            sp = in(reg) sp,
            f = in(reg) overflowed as *const () as usize,
            in("a6") pc,
            in("a8") min,
            in("a9") max,
            in("a10") u32::from(sp < min),
            in("a11") 0,
            options(noreturn),
        );
    }
}

extern "C" fn overflowed(pc: u32, sp: usize, min: u32, max: u32, below: u32) -> ! {
    panic!(
        "[{:?}] Stack overflow: SP {} its stack's bounds [0x{:08x}, 0x{:08x}] at PC 0x{:08x} (SP 0x{:08x} when reported)",
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
