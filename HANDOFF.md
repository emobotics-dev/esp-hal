# HANDOFF: `local-new` vs `local-1.1.1`

For the HIL agent. This is the emobotics **esp-hal 1.1.1 fork**, not upstream main.

Rebuilt from `esp-hal-v1.1.1`. Follow-ups squashed into the originating commit. Where we dropped fork-local work that main already fixed, the replacement commit uses that PR’s author/subject (1.1.1 paths — main’s patches do not apply as-is).

| | |
|---|---|
| Branch | `local-new` |
| Forked from | `esp-hal-v1.1.1` (`976adef27`) |
| Worktree | `/home/holger-local/worktrees/analyse-changes/local-new` |
| Host check | `esp32c3` + `esp32` `cargo check` were green on this tree. **No HIL yet.** |

Do **not** re-add dropped APIs or the ROM-stack reclaim to “make fire27 link.” If fire27 cannot place `LVGL_BUFS`, that is an **application** layout problem.

---

## History vs `local-1.1.1` — what to send upstream

Linear from `esp-hal-v1.1.1`. One topic per commit. No follow-up “align” commits.

### From main (1.1.1 backport — patches do not apply to main’s paths)

| Commit | Replaces (`local-1.1.1`) |
|---|---|
| `feat(spi): zero-copy DMA…` (includes #5290 unaligned **copy** fallback) | `ff50cc303` + pad-chain `94a27b06e` |
| `USB-serial: prevent async data loss… (#6104)` also #6089 + #6097 | wait-before-stuff `9b37236e2` + lock as a separate follow-up |
| `radio: remove asm_experimental_arch (#5653)` | same one-liner as `2ceb142a2` |
| `Fix ESP32 SPI hang (#6107)` | `trans_done.set_bit()` `51e063dbf` |

`#5290` / `#6089` / `#6104` / `#6097` / `#6107` cannot be `git cherry-pick`’d: main moved `usb_serial_jtag.rs`, split SPI into `low_level/`, and `#5290` is a 1100-line scoped-buffer refactor. Content is the 1.1.1-shaped equivalent.

### Unique fork commits (keep; send upstream as our PRs)

Logger, `.bss.radio` (esp32-only), stack **min** (not #6139’s all-chip default-8192), DBREAKC, exception dump, OOM log, RX dscr recover, UART FIFO bound, busy-poll budget, GDMA FIFO enum, command-phase swap, I2C comment, rtos `stack_usage`, S3 `.rotext_dummy`, SPI slave IDF sequence, aliasing UB, async fence/quiesce (no debug counters), Cell→atomic + type assert.

### Dropped from history (do not restore)

Pad-chain, ROM-stack reclaim, I2C NACK skip/rate-limit, `prepare_for_tx` import gate, `descriptor_address`, `arm_half_duplex_*` / `take_transfer`, debug counters, `MAIN_STACK_MAX_SIZE`, mega-align commit.

---

## What to point the app at

Consumer (`alternator-regulator` / `m5stack-core`) `esp-hal` path-dep / submodule: **`local-new`**, not `local-1.1.1`.

Expect **compile breaks** if the app used:

- `SpiDma::arm_half_duplex_read` / `arm_half_duplex_write`
- `SpiDma::take_transfer` / `transfer_done`
- `SpiDma::listen_dma_{rx,tx}` / `clear_dma_*` / `pending_dma_*`
- `DmaRxBuf::descriptor_address` / `DmaTxBuf::descriptor_address`

Public surface is `half_duplex_*` / `SpiDmaTransfer::{wait,is_done}`. Channel `listen_in` / `clear_in` still exist.

`ESP_HAL_CONFIG_MAIN_STACK_MAX_SIZE` is **gone**. `MAIN_STACK_MIN_SIZE` still works (esp32-only, default 0).

---

## Behaviour that **changed** vs `local-1.1.1` (must re-HIL)

Same four items as before the rewrite — tree did not change.

### 1. ESP32 SPI master DMA TX, unaligned lengths

Zero-copy only if DRAM **and** `addr % 4 == 0` **and** `len % 4 == 0`. Else copy into `tx_buf` (one continuous burst). No pad-chain, no length round-up.

**HIL (fire27):** SD `:format` / `:O` / `:o` (6-byte commands, unaligned FAT). Display flush. No silent wedge, no mid-frame clock gap. RX dscr-fault / `BUSY_POLL_BUDGET` / `cancel_and_quiesce` still present.

### 2. ESP32 `enable_listen` / TransferDone race

Plain RMW again. `SpiFuture` and DMA idle Fut **re-check after arming** (#6107). Busy-poll budget still there.

**HIL (fire27):** SPI + BLE + SD + display. Old symptom: `usr` clear, status 0, waiter parked, `with_timeout` dead. Do not restore `trans_done.set_bit()`.

### 3. USB-serial-jtag async write (cores3)

Stuff → `wr_done` → `wait_tx_ready` (new `IN_EMPTY`, then `data_free`). Flush sends ZLP. Write future is enable-bit only.

Unchanged: `INT_ENA_LOCK`, ISR clears **observed** bits, read future `data_avail()`.

**HIL (cores3):** bidirectional console; writes of length `N*64`; no deaf reader.

### 4. ESP32 memory map / linker

Both ROM stacks reserved. `dram2_seg` origin `0x3FFE_7E30`, len **98 768** (−11 KiB vs `local-1.1.1`). `.bss` ALIGN 4. `.bss.radio` only `#IF esp32`. Stack **min** only.

**HIL / link (fire27 first):** `LVGL_BUFS` + BLE heap may overflow `dram2`. **Do not reclaim ROM stack.** Move buffers in the app. BLE must still come up; blob symbols must stay put when user `.bss` grows.

---

## Still fork-only — regression-test, don’t “fix”

| Area | Still on `local-new` | HIL note |
|---|---|---|
| SPI command phase | `HAL_SPI_SWAP_DATA_TX` | Width 8 identity. |
| `State` / `DmaState` | `Atomic*`; type assert | `Cell` revert fails the build. |
| GDMA FIFO IRQs | `FifoOverflow` / `FifoUnderflow` | cores3 `clear_all()`. |
| SPI slave ESP32 | CS freeze, DMA-before-usr, bitlen=max | #21 still open. |
| I2C NACK | 1.1.1 `reset_fsm` | cores3 absent FT6336U/PPS. |
| DBREAKC | `0b11` | fire27 parallel `:format`. |
| UART print ESP32 | FIFO wait, drop on budget | BLE vs log flood. |
| rtos `stack_usage` | yes | not a wire change. |

---

## Suggested HIL order

1. **Link fire27.** Overflow → record sizes, stop, don’t patch `memory.x`.
2. fire27 boot: BLE, UI, no stack-guard panic.
3. fire27 SD: `:format` solo, then parallel, then `:O` / `:o`.
4. fire27 display FPS vs a `local-1.1.1` baseline if you have one.
5. cores3 touch + AXP cold boot.
6. cores3 USB console bidirectional + 64-byte-multiple writes.
7. SPI slave on fire27 only if the app still uses it.

Classify failures:

- Missing `arm_*` → app migrates to `SpiDmaTransfer`.
- `dram2` overflow → app layout.
- SD hang / parked waiter → (1) or (2). Capture `usr`, `trans_done`, `inlink_dscr_error`.
- USB deaf/mute → (3).
- BLE wedge after `.bss` change → compare `phy_rxrf_dc` etc. to `local-1.1.1`.

---

## Do not re-introduce

- `arm_half_duplex_*` / `take_transfer` / `descriptor_address`.
- Reclaiming `reserved_rom_stack_app`.
- `prepare_for_tx` rounding `length` up without a pad buffer.
- I2C NACK skip / rate-limit “to protect the radio”.
- `enable_listen` writing `trans_done = 1` **and** the #6107 re-check stacked.
- USB write-future completing on `data_free` **together** with `wait_tx_ready`.
- A follow-up “align” mega-commit. Fix the originating commit.
