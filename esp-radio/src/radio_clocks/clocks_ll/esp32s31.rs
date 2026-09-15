pub(crate) fn enable_wifi(en: bool) {
    // ESP-IDF's WIFI_CLOCK_DEPS, through the reference counts esp-phy keeps: the PHY
    // holds the baseband, its APB bus and the PLL source gate too, so writing them
    // here directly gated clocks the other side still used. That also carries the
    // baseband reset, which ESP-IDF pulses (1 then 0) -- this used to set it only.
    // The Wi-Fi power clock is not among the dependencies: esp-hal selects and
    // enables it once at boot, as ESP-IDF's `modem_clock_select_lp_clock_source`.
    if en {
        esp_phy::modem_clock::enable(esp_phy::modem_clock::WIFI);
    } else {
        esp_phy::modem_clock::disable(esp_phy::modem_clock::WIFI);
    }
}

pub(crate) fn enable_bt(_en: bool) {
    // Espressif ships no `btdm_app` blob for this chip yet, so esp-radio cannot
    // drive its Bluetooth. Left unimplemented rather than guessed at.
}

pub(crate) fn enable_ieee802154(_en: bool) {
    // The ESP32-S31 has an IEEE 802.15.4 peripheral, but esp-radio has no
    // support for it on this chip yet.
}

pub(crate) fn reset_wifi_mac() {
    // empty
}

pub(crate) fn init_clocks() {
    // done in esp-hal
}

pub(crate) fn deinit_clocks() {
    // nothing to do, `init_clocks` is a no-op
}

pub(crate) fn ble_rtc_clk_init() {
    // nothing for this target (yet)
}

pub(crate) fn reset_rpa() {
    // nothing for this target (yet)
}
