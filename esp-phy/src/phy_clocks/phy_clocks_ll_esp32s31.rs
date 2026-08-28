/// Bits of `MODEM_SYSCON.clk_conf1` that ESP-IDF's `MODEM_CLOCK_WIFI_BB`
/// enables as one masked write (`modem_syscon_ll_clk_wifibb_configure`).
const WIFI_BB_CLK_CONF1: u32 = 0x17b;

/// `HP_SYS_CLKRST.modem_conf` values for `SOC_PLL_SOURCE_CG`, taken verbatim
/// from `modem_clock_hal_enable_soc_pll_source_cg`.
const MODEM_CONF_PLL_ON: u32 = 0x3d;
const MODEM_CONF_PLL_OFF: u32 = 0x25;

pub(crate) fn enable_phy(en: bool) {
    // Only the root gate. The ESP32-S31 sets `SOC_CLK_ANA_I2C_MST_HAS_ROOT_GATE`
    // but not `SOC_CLK_ANA_I2C_MST_DEPENDS_ON_MODEM_APB`, so ESP-IDF's
    // `ANA_I2C_SRC_CLOCK_ENABLE` compiles away and `ANALOG_CLOCK_ENABLE`
    // reduces to `regi2c_ctrl_ll_master_enable_clock`.
    //
    // Unlike the C6/C5/C61 there is no `i2c_mst_clk_conf`, and so no
    // `clk_i2c_mst_sel_160m` step to go with it.
    regs!(MODEM_LPCON)
        .clk_conf()
        .modify(|_, w| w.clk_i2c_mst_en().bit(en));

    // The root gate alone is not enough. ESP-IDF's `esp_phy_enable` also calls
    // `phy_module_enable()`, which on this chip is
    // `modem_clock_module_enable(PERIPH_PHY_CALIBRATION_MODULE)`, and then
    // asserts `phy_module_has_clock_bits(0x38E5FF)`. Without it the PHY and
    // baseband are unclocked and `register_chipv7_phy` never returns -- it
    // hangs, silently, on the first Wi-Fi call.
    //
    // The sets come from `esp_hw_support/modem/port/esp32s31/
    // modem_clock_impl.c`:
    //
    //   PHY_CLOCK_DEPS             MODEM_ADC_COMMON_FE, MODEM_PRIVATE_FE,
    //                              SOC_PLL_SOURCE_CG, I2C_MASTER
    //   PHY_CALIBRATION_CLOCK_DEPS WIFI_APB, WIFI_BB, WIFI_BB_44M,
    //                              WIFI_BB_80X1, BT_I154_COMMON_BB, BT_APB,
    //                              SOC_PLL_SOURCE_CG
    //
    // and each clock's register write from the matching
    // `modem_syscon_ll_enable_*` in `hal/esp32s31/include/hal/
    // modem_syscon_ll.h`. Transcribed, not inferred.
    let syscon = regs!(MODEM_SYSCON);
    syscon.clk_conf1().modify(|r, w| {
        // MODEM_CLOCK_WIFI_BB: one masked write in ESP-IDF, kept as one here.
        let bits = if en { r.bits() | WIFI_BB_CLK_CONF1 } else { r.bits() & !WIFI_BB_CLK_CONF1 };
        // SAFETY: writing the same field set ESP-IDF writes, on the register
        // the PAC models; the mask is ESP-IDF's own.
        unsafe { w.bits(bits) }
            // PHY_CALIBRATION_WIFI_CLOCK_DEPS
            .clk_wifi_apb_en()
            .bit(en)
            .clk_wifibb_44m_en()
            .bit(en)
            .clk_wifibb_80x1_en()
            .bit(en)
            // PHY_CALIBRATION_BT_I154_CLOCK_DEPS
            .clk_btbb_en()
            .bit(en)
            .clk_bt_apb_en()
            .bit(en)
            // MODEM_PRIVATE_FE
            .clk_fe_160m_en()
            .bit(en)
            .clk_fe_dac_en()
            .bit(en)
            .clk_fe_pwdet_adc_en()
            .bit(en)
            .clk_fe_adc_en()
            .bit(en)
            // MODEM_ADC_COMMON_FE
            .clk_fe_apb_en()
            .bit(en)
            .clk_fe_80m_en()
            .bit(en)
    });

    // SOC_PLL_SOURCE_CG. ESP-IDF writes the whole register, both values fixed.
    // SAFETY: the two constants are ESP-IDF's, for this exact register.
    regs!(HP_SYS_CLKRST)
        .modem_conf()
        .write(|w| unsafe { w.bits(if en { MODEM_CONF_PLL_ON } else { MODEM_CONF_PLL_OFF }) });
}
