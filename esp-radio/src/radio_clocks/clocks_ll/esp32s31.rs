pub(crate) fn enable_wifi(en: bool) {
    // SOC_PLL_SOURCE_CG. ESP-IDF writes the register whole rather than a field
    // (`modem_clock_hal_enable_soc_pll_source_cg`, esp32s31/modem_clock_hal.c).
    regs!(HP_SYS_CLKRST)
        .modem_conf()
        .write(|w| unsafe { w.bits(if en { 0x3d } else { 0x25 }) });

    if en {
        // The baseband is reset before its clocks are ungated
        // (`modem_clock_wifi_bb_configure`).
        regs!(MODEM_SYSCON)
            .modem_rst_conf()
            .modify(|_, w| w.rst_wifibb().set_bit());
    }

    // ESP-IDF's WIFI_CLOCK_DEPS: WIFI_MAC, WIFI_APB, WIFI_BB, WIFI_BB_44M,
    // WIFI_BB_80X1, COEXIST, SOC_PLL_SOURCE_CG.
    regs!(MODEM_SYSCON).clk_conf1().modify(|_, w| {
        w.clk_wifimac_en().bit(en);
        w.clk_wifi_apb_en().bit(en);
        // WIFI_BB is the group `modem_syscon_ll_clk_wifibb_configure` writes as
        // the mask 0x17b; spelled out by name here.
        w.clk_wifibb_22m_en().bit(en);
        w.clk_wifibb_40m_en().bit(en);
        w.clk_wifibb_80m_en().bit(en);
        w.clk_wifibb_40x_en().bit(en);
        w.clk_wifibb_80x_en().bit(en);
        w.clk_wifibb_40x1_en().bit(en);
        w.clk_wifibb_160x1_en().bit(en);
        // Their own dependencies, gated separately by ESP-IDF.
        w.clk_wifibb_44m_en().bit(en);
        w.clk_wifibb_80x1_en().bit(en)
    });

    regs!(MODEM_LPCON).clk_conf().modify(|_, w| {
        w.clk_wifipwr_en().bit(en);
        w.clk_coex_en().bit(en)
    });
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
