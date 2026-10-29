//! The contract with a stack-pointer monitor driver.
//!
//! A task switch loads the next task's SP in the level-1 exit, so that is where
//! a monitor guarding the stack being entered has to change bounds. If the
//! program defines [`SYMBOL`], the exit treats it as an array of per-core
//! blocks, indexed by `PRID` bit 13 and [`STRIDE_SHIFT`] bytes apart, laid out
//! as the offsets below. With the block armed, the exit stops the monitor,
//! writes the staged bounds, loads SP, then drops any latch and restarts it.
//! Without the symbol, it loads SP and nothing else.

/// The symbol the level-1 exit looks for; `exception.x` makes it 0 when no
/// driver defines it.
pub const SYMBOL: &str = "__xtensa_lx_rt_stack_monitor";

/// Non-zero once the monitor guards this core.
pub const ARMED: usize = 0;
/// Address of the monitor's enable register.
pub const MONTR_ENA: usize = 4;
/// Enable-register value with the SP monitor stopped.
pub const MONTR_STOPPED: usize = 8;
/// Enable-register value with the SP monitor running.
pub const MONTR_RUNNING: usize = 12;
/// Address of the lower-bound register.
pub const SP_MIN: usize = 16;
/// Address of the upper-bound register.
pub const SP_MAX: usize = 20;
/// Address of the interrupt-clear register.
pub const INTR_CLR: usize = 24;
/// Value that drops the monitor's latch.
pub const CLEAR_BITS: usize = 28;
/// Lower bound the next exit applies.
pub const NEXT_MIN: usize = 32;
/// Upper bound the next exit applies.
pub const NEXT_MAX: usize = 36;
/// `log2` of the stride between cores' blocks.
pub const STRIDE_SHIFT: usize = 6;
