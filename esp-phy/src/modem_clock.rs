//! The ESP32-S31's modem clock devices, reference-counted and shared by every radio user.
//!
//! This is ESP-IDF's `esp_hw_support/modem/modem_clock.c` with
//! `modem/port/esp32s31/modem_clock_impl.c`, transcribed. The PHY (this crate) and the Wi-Fi
//! driver (`esp-radio`) depend on overlapping sets of these devices -- the baseband, its APB bus
//! and the PLL source gate among them -- so neither side may gate a device without knowing whether
//! the other still needs it. Gating them unconditionally from both sides stalled the CPU: the Wi-Fi
//! blob still reads baseband registers (ROM `phy_get_max_pwr`) after `phy_disable` and
//! `wifi_clock_disable`, and a bus read from an unclocked peripheral never completes.
//!
//! ESP-IDF adds a rule on top of the reference counts: while Wi-Fi is initialized, the Wi-Fi MAC,
//! baseband and baseband APB clocks are never gated (`MODEM_STATUS_WIFI_INITED`). The blob relies
//! on it, so it is kept here as [set_wifi_initialized].

use core::cell::RefCell;

use esp_sync::RawMutex;

/// One clock device, in ESP-IDF's `modem_clock_device_t` order. Devices are configured in this
/// order, which is what puts the baseband reset pulse in front of its clock gates.
#[derive(Clone, Copy)]
enum Device {
    ModemAdcCommonFe,
    ModemPrivateFe,
    SocPllSourceCg,
    Coexist,
    I2cMaster,
    WifiApb,
    WifiBb44m,
    WifiMac,
    WifiBb,
    WifiBb80x1,
    BtApb,
    BtI154CommonBb,
}

impl Device {
    const ALL: [Device; 12] = [
        Device::ModemAdcCommonFe,
        Device::ModemPrivateFe,
        Device::SocPllSourceCg,
        Device::Coexist,
        Device::I2cMaster,
        Device::WifiApb,
        Device::WifiBb44m,
        Device::WifiMac,
        Device::WifiBb,
        Device::WifiBb80x1,
        Device::BtApb,
        Device::BtI154CommonBb,
    ];

    const fn bit(self) -> u16 {
        1 << self as u16
    }

    /// ESP-IDF counts every device except the analog I2C master, whose gate follows each call.
    const fn counted(self) -> bool {
        !matches!(self, Device::I2cMaster)
    }
}

/// A set of devices one user needs, like ESP-IDF's `modem_clock_get_module_deps`.
#[derive(Clone, Copy)]
pub struct Module(u16);

impl Module {
    const fn of(devices: &[Device]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < devices.len() {
            bits |= devices[i].bit();
            i += 1;
        }
        Self(bits)
    }
}

/// `WIFI_CLOCK_DEPS`: what the Wi-Fi driver holds between `wifi_clock_enable` and
/// `wifi_clock_disable`.
pub const WIFI: Module = Module::of(&[
    Device::WifiMac,
    Device::WifiApb,
    Device::WifiBb,
    Device::WifiBb44m,
    Device::Coexist,
    Device::WifiBb80x1,
    Device::SocPllSourceCg,
]);

/// `PHY_CLOCK_DEPS`.
pub(crate) const PHY: Module = Module::of(&[
    Device::ModemAdcCommonFe,
    Device::ModemPrivateFe,
    Device::SocPllSourceCg,
    Device::I2cMaster,
]);

/// `PHY_CALIBRATION_CLOCK_DEPS`, the union of its Wi-Fi and BT/802.15.4 halves.
pub(crate) const PHY_CALIBRATION: Module = Module::of(&[
    Device::WifiApb,
    Device::WifiBb,
    Device::WifiBb44m,
    Device::WifiBb80x1,
    Device::SocPllSourceCg,
    Device::BtI154CommonBb,
    Device::BtApb,
]);

struct State {
    refs: [u8; Device::ALL.len()],
    wifi_initialized: bool,
}

static STATE: embassy_sync::blocking_mutex::Mutex<RawMutex, RefCell<State>> =
    embassy_sync::blocking_mutex::Mutex::new(RefCell::new(State {
        refs: [0; Device::ALL.len()],
        wifi_initialized: false,
    }));

/// Acquire every device of `module`, ungating those nobody held.
pub fn enable(module: Module) {
    control(module, true);
}

/// Release every device of `module`, gating those nobody else holds.
pub fn disable(module: Module) {
    control(module, false);
}

/// Record whether Wi-Fi is initialized, as ESP-IDF's `modem_clock_configure_wifi_status`. While it
/// is, the Wi-Fi MAC, baseband and baseband APB clocks stay ungated even at a count of zero.
pub fn set_wifi_initialized(initialized: bool) {
    STATE.lock(|state| state.borrow_mut().wifi_initialized = initialized);
}

fn control(module: Module, enable: bool) {
    STATE.lock(|state| {
        let mut state = state.borrow_mut();
        let wifi_initialized = state.wifi_initialized;
        for device in Device::ALL {
            if module.0 & device.bit() == 0 {
                continue;
            }
            // ESP-IDF's `modem_clock_device_config_wrapper`: enabling acts on the first holder,
            // disabling on the last.
            let act = if !device.counted() {
                true
            } else if enable {
                let count = &mut state.refs[device as usize];
                let held = *count;
                *count = unwrap!(
                    held.checked_add(1),
                    "modem clock reference count overflowed"
                );
                held == 0
            } else {
                let count = &mut state.refs[device as usize];
                *count = unwrap!(
                    count.checked_sub(1),
                    "modem clock released more often than it was acquired"
                );
                *count == 0
            };
            if act {
                configure(device, enable, wifi_initialized);
            }
        }
    });
}

/// One device's register writes, from the matching `modem_clock_*_configure` in
/// `modem_clock_impl.c` and the `modem_syscon_ll`/`modem_lpcon_ll` helpers it calls.
fn configure(device: Device, en: bool, wifi_initialized: bool) {
    let syscon = regs!(MODEM_SYSCON);
    let lpcon = regs!(MODEM_LPCON);
    // `if (enable || !(ctx->modem_status & MODEM_STATUS_WIFI_INITED))`
    let wifi_may_change = en || !wifi_initialized;

    match device {
        // The two FE devices are only ever turned on; ESP-IDF's HAL ignores a disable.
        Device::ModemAdcCommonFe => {
            if en {
                syscon
                    .clk_conf1()
                    .modify(|_, w| w.clk_fe_apb_en().set_bit().clk_fe_80m_en().set_bit());
            }
        }
        Device::ModemPrivateFe => {
            if en {
                syscon.clk_conf1().modify(|_, w| {
                    w.clk_fe_160m_en()
                        .set_bit()
                        .clk_fe_dac_en()
                        .set_bit()
                        .clk_fe_pwdet_adc_en()
                        .set_bit()
                        .clk_fe_adc_en()
                        .set_bit()
                });
            }
        }
        // `modem_clock_hal_enable_soc_pll_source_cg` writes the whole register, both values fixed.
        Device::SocPllSourceCg => {
            const MODEM_CONF_PLL_ON: u32 = 0x3d;
            const MODEM_CONF_PLL_OFF: u32 = 0x25;
            regs!(HP_SYS_CLKRST)
                .modem_conf()
                // SAFETY: ESP-IDF's two constants, for this exact register.
                .write(|w| unsafe {
                    w.bits(if en {
                        MODEM_CONF_PLL_ON
                    } else {
                        MODEM_CONF_PLL_OFF
                    })
                });
        }
        Device::Coexist => {
            lpcon.clk_conf().modify(|_, w| w.clk_coex_en().bit(en));
        }
        Device::I2cMaster => {
            lpcon.clk_conf().modify(|_, w| w.clk_i2c_mst_en().bit(en));
        }
        Device::WifiApb => {
            if wifi_may_change {
                syscon
                    .clk_conf1()
                    .modify(|_, w| w.clk_wifi_apb_en().bit(en));
            }
        }
        Device::WifiBb44m => {
            if wifi_may_change {
                syscon
                    .clk_conf1()
                    .modify(|_, w| w.clk_wifibb_44m_en().bit(en));
            }
        }
        Device::WifiMac => {
            if wifi_may_change {
                syscon.clk_conf1().modify(|_, w| w.clk_wifimac_en().bit(en));
            }
        }
        Device::WifiBb => {
            if wifi_may_change {
                if en {
                    // `modem_syscon_ll_reset_wifibb` is a pulse.
                    syscon
                        .modem_rst_conf()
                        .modify(|_, w| w.rst_wifibb().set_bit());
                    syscon
                        .modem_rst_conf()
                        .modify(|_, w| w.rst_wifibb().clear_bit());
                }
                // `modem_syscon_ll_clk_wifibb_configure`: 22M/40M/80M/40X/80X/40X1/160X1 as the
                // one mask 0x17b; 44M and 80X1 are devices of their own.
                const WIFI_BB_MASK: u32 = 0x17b;
                syscon.clk_conf1().modify(|r, w| {
                    let bits = if en {
                        r.bits() | WIFI_BB_MASK
                    } else {
                        r.bits() & !WIFI_BB_MASK
                    };
                    // SAFETY: only ESP-IDF's mask changes; every other bit is written back.
                    unsafe { w.bits(bits) }
                });
            }
        }
        Device::WifiBb80x1 => {
            if wifi_may_change {
                syscon
                    .clk_conf1()
                    .modify(|_, w| w.clk_wifibb_80x1_en().bit(en));
            }
        }
        Device::BtApb => {
            syscon.clk_conf1().modify(|_, w| w.clk_bt_apb_en().bit(en));
            syscon
                .clk_conf()
                .modify(|_, w| w.clk_modem_sec_apb_en().bit(en));
        }
        Device::BtI154CommonBb => {
            syscon.clk_conf1().modify(|_, w| w.clk_btbb_en().bit(en));
        }
    }
}
