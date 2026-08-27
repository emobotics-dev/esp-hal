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
}
