use crate::trapframe::TrapFrame;

#[cfg(xtensa)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".rwtext")]
unsafe extern "C" fn __user_exception(
    cause: xtensa_lx_rt::exception::ExceptionCause,
    context: &TrapFrame,
) {
    panic!(
        "\n\nException occurred on {:?} '{:?}'\n{:?}",
        crate::system::Cpu::current(),
        cause,
        context
    );
}

#[cfg(all(xtensa, stack_guard_monitoring))]
pub(crate) fn breakpoint_interrupt(context: &TrapFrame) {
    let mut dbgcause: u32;
    let mut dbreaka0: u32;
    let mut dbreakc0: u32;
    unsafe {
        core::arch::asm!(
            "rsr.debugcause {0}",
            "rsr {1}, 144",   // DBREAKA0 = SR 144
            "rsr {2}, 160",   // DBREAKC0 = SR 160
            out(reg) dbgcause,
            out(reg) dbreaka0,
            out(reg) dbreakc0,
            options(nostack)
        );
    }

    #[cfg(stack_guard_monitoring)]
    if dbgcause & 4 != 0 && dbgcause & 0b1111_0000_0000 == 0 {
        // Watchpoint trip: DBREAKA0 holds the watched address (the current
        // task's stack-guard); compare context.A1 to infer how deep that
        // task was when its guard got written.
        panic!(
            "\n\nDetected a write to the stack guard value on {:?}\nDBREAKA0=0x{:08x} DBREAKC0=0x{:08x} task_sp=0x{:08x} sp_to_guard={}\n{:?}",
            crate::system::Cpu::current(),
            dbreaka0,
            dbreakc0,
            context.A1,
            context.A1.wrapping_sub(dbreaka0) as i32,
            context
        );
    }

    panic!(
        "\n\nBreakpoint on {:?}\n{:?}\nDebug cause: {} DBREAKA0=0x{:08x} DBREAKC0=0x{:08x}",
        crate::system::Cpu::current(),
        context,
        dbgcause,
        dbreaka0,
        dbreakc0,
    );
}

#[cfg(riscv)]
#[unsafe(no_mangle)]
unsafe extern "C" fn ExceptionHandler(context: &TrapFrame) -> ! {
    let mepc = riscv::register::mepc::read();
    let code = riscv::register::mcause::read().code();
    let mtval = riscv::register::mtval::read();

    unsafe extern "C" {
        static mut __stack_chk_guard: u32;
    }

    if code == 14 {
        panic!(
            "Stack overflow detected at 0x{:x}, possibly called by 0x{:x}",
            mepc, context.ra
        );
    }

    #[cfg(stack_guard_monitoring)]
    if code == 3 {
        let guard_addr = core::ptr::addr_of_mut!(__stack_chk_guard) as *mut _ as usize;

        if mtval == guard_addr {
            panic!(
                "Detected a write to the main stack's guard value at 0x{:x}, possibly called by 0x{:x}",
                mepc, context.ra
            )
        } else {
            if unsafe { crate::debugger::watchpoint_hit(1) } {
                panic!(
                    "Detected a write to the trap/rwtext segment at 0x{:x}, possibly called by 0x{:x}",
                    mepc, context.ra
                );
            } else if unsafe { crate::debugger::watchpoint_hit(0) } {
                panic!(
                    "Detected a write to a stack guard value at 0x{:x}, possibly called by 0x{:x}",
                    mepc, context.ra
                );
            } else {
                panic!(
                    "Breakpoint exception at 0x{:08x}, mtval=0x{:08x}\n{:?}",
                    mepc, mtval, context
                );
            }
        }
    }

    let code = match code {
        0 => "Instruction address misaligned",
        1 => "Instruction access fault",
        2 => "Illegal instruction",
        3 => "Breakpoint",
        4 => "Load address misaligned",
        5 => "Load access fault",
        6 => "Store/AMO address misaligned",
        7 => "Store/AMO access fault",
        8 => "Environment call from U-mode",
        9 => "Environment call from S-mode",
        10 => "Reserved",
        11 => "Environment call from M-mode",
        12 => "Instruction page fault",
        13 => "Load page fault",
        14 => "Reserved",
        15 => "Store/AMO page fault",
        _ => "UNKNOWN",
    };

    panic!(
        "Exception '{}' mepc=0x{:08x}, mtval=0x{:08x}\n{:?}",
        code, mepc, mtval, context
    );
}
