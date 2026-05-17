SECTIONS {
  /* must be last segment using RWDATA */
  .stack (NOLOAD) : ALIGN(4)
  {
    _stack_end = ABSOLUTE(.);
    _stack_end_cpu0 = ABSOLUTE(.);

    /* The stack_guard for `stack-protector` mitigation - https://doc.rust-lang.org/rustc/exploit-mitigations.html#stack-smashing-protection */
    __stack_chk_guard = ABSOLUTE(_stack_end) + ${ESP_HAL_CONFIG_STACK_GUARD_OFFSET};

    ASSERT(_stack_start - _stack_end >= ${ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM}, "Main stack is smaller than ${ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM} bytes.");

    . = ORIGIN(RWDATA) + LENGTH(RWDATA);

    . = ALIGN (4);
    _stack_start = ABSOLUTE(.);
    _stack_start_cpu0 = ABSOLUTE(.);
  } > RWDATA
}

/* Lower bound only — matches upstream #6139 (`ensure-main-stack-minimum`).
   An upper bound is an application DRAM-layout lock, not a HAL primitive.
   Scoped to esp32: the BLE-blob hang it guards is ESP32-specific, and
   other chips inheriting the env from a workspace .cargo/config.toml
   would false-positive. Default 0 disables the check. */
#IF esp32
ASSERT(
  ${ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE} == 0
    || (_stack_start_cpu0 - _stack_end_cpu0) >= ${ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE},
  "main task stack region is smaller than ESP_HAL_CONFIG_MAIN_STACK_MIN_SIZE — \
reduce .bss/.noinit usage or lower the configured minimum"
);
#ENDIF
