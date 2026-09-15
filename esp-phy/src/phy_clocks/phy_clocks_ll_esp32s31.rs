use crate::modem_clock;

pub(crate) fn enable_phy(en: bool) {
    // ESP-IDF's `esp_phy_enable` takes two modules: `PERIPH_PHY_MODULE` through
    // `esp_phy_common_clock_enable`, and `PERIPH_PHY_CALIBRATION_MODULE` through
    // `phy_module_enable`, whose clocks `register_chipv7_phy` needs or it never
    // returns. Both go through the shared reference counts, because the Wi-Fi
    // driver holds the baseband, its APB bus and the PLL source gate as well:
    // gating them here unconditionally stalled the CPU in a later baseband read.
    //
    // ESP-IDF drops the calibration module again once calibration is done; it is
    // held for the PHY's whole lifetime here, which keeps clocks on longer but
    // never gates one a user still needs.
    if en {
        modem_clock::enable(modem_clock::PHY);
        modem_clock::enable(modem_clock::PHY_CALIBRATION);
    } else {
        modem_clock::disable(modem_clock::PHY_CALIBRATION);
        modem_clock::disable(modem_clock::PHY);
    }
}
