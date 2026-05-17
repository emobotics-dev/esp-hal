SECTIONS {
  /* must be last segment using RWDATA */
  .stack (NOLOAD) : ALIGN(4)
  {
    _stack_end = ABSOLUTE(.);
    _stack_end_cpu0 = ABSOLUTE(.);

    /* The stack_guard for `stack-protector` mitigation - https://doc.rust-lang.org/rustc/exploit-mitigations.html#stack-smashing-protection */
    __stack_chk_guard = ABSOLUTE(_stack_end) + ${ESP_HAL_CONFIG_STACK_GUARD_OFFSET};

    . = ORIGIN(RWDATA) + LENGTH(RWDATA);

    . = ALIGN (4);
    _stack_start = ABSOLUTE(.);
    _stack_start_cpu0 = ABSOLUTE(.);
  } > RWDATA
}

/* Compile-time stack-size assertion (lower bound). Builds with too much `.bss`
   would shrink the main task stack below the configured minimum, which on
   ESP32 + BLE silently hangs `btdm_app` init at boot. Default 0 disables the
   check; set ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE to a defensive value to
   convert that silent runtime failure into a clear link-time error. */
ASSERT(
  ${ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE} == 0
    || (_stack_start_cpu0 - _stack_end_cpu0) >= ${ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE},
  "main task stack region is smaller than ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE — \
reduce .bss/.noinit usage or lower the configured minimum"
);

/* Compile-time stack-size assertion (upper bound). On ESP32 + BLE the chip
   silently wedges BLE controller init when the main task stack region grows
   past an empirically validated ceiling, even though the boot stack just has
   more headroom: the failure mode is not stack overflow but some address-
   dependent interaction with the BLE controller blob's expected DRAM layout.
   Combined with stack-guard-monitoring on ESP32 (DBREAKA0 + STORE watchpoint
   on `__stack_chk_guard` at `_stack_end + STACK_GUARD_OFFSET`), keeping the
   stack region bounded both above and below pins the controller blob's
   working set in a known-tested layout. Default 0 disables the check; set
   ESP_HAL_CONFIG_MAIN_STACK_MAX_SIZE to a defensive value to convert the
   silent runtime wedge into a clear link-time error. */
ASSERT(
  ${ESP_HAL_CONFIG_MAIN_STACK_MAX_SIZE} == 0
    || (_stack_start_cpu0 - _stack_end_cpu0) <= ${ESP_HAL_CONFIG_MAIN_STACK_MAX_SIZE},
  "main task stack region is larger than ESP_HAL_CONFIG_MAIN_STACK_MAX_SIZE — \
grow .bss/.noinit usage or raise the configured maximum"
);
