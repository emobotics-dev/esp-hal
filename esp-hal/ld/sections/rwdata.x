.data : ALIGN(4)
{
  _data_start = ABSOLUTE(.);
  . = ALIGN (4);

  #IF ESP_HAL_CONFIG_PLACE_SWITCH_TABLES_IN_RAM
    *(.rodata.*_esp_hal_internal_handler*)
    *(.rodata..Lswitch.table.*)
    *(.rodata.cst*)
  #ENDIF

  #IF ESP_HAL_CONFIG_PLACE_ANON_IN_RAM
    *(.rodata..Lanon .rodata..Lanon.*)
  #ENDIF

  #IF ESP_HAL_CONFIG_USE_RWDATA_LD_HOOK
    INCLUDE "rwdata_hook.x"
  #ENDIF

  *(.sdata .sdata.* .sdata2 .sdata2.*);
  *(.data .data.*);
  *(.data1)
  _data_end = ABSOLUTE(.);
  . = ALIGN(4);
} > RWDATA

/* LMA of .data */
_sidata = LOADADDR(.data);

.data.wifi :
{
  . = ALIGN(4);
  *( .dram1 .dram1.*)
  . = ALIGN(4);
} > RWDATA

.bss (NOLOAD) : ALIGN(16)
{
  _bss_start = ABSOLUTE(.);
  . = ALIGN(16);

  /* Radio-blob `.bss` placed first inside `.bss`, so user-`.bss` growth
     extends only after `_bss_radio_end`. The blob's symbol addresses
     (phy_*, chip7_*, bt_wifi_chan_data, etc.) therefore depend only on
     the size of `.data*` and the blobs themselves — both stable across
     application code changes. Pre-fix, a 2 KB user-`.bss` growth shifted
     blob addresses by 0x980 and broke BLE init silently on ESP32 (LX6).
     See `project_fire27_layout_fragility_persists` memory note. The blob
     uses R_XTENSA_32 relocations so any DRAM address works at link time;
     stability across builds is what the blob actually needs.
     Archives: every static-library shipped by `esp-wifi-sys-{esp32,...}`
     with `.bss` content (printf/regulatory/wapi have none today —
     listed for forward-compat, harmless if empty).
     The `.bss` start marker stays at the very beginning so xtensa-lx-rt's
     Reset still zeroes blob + user .bss as one contiguous block. */
  /* COMMON is critical here: libphy.a, libpp.a, libbtdm_app.a etc. are
     pre-compiled without `-fno-common`, so their globals (phy_rxrf_dc,
     chip7_sleep_params, bt_wifi_chan_data, etc.) are emitted as COMMON
     symbols rather than `.bss.*`. The catch-all `*(COMMON)` rule below
     would otherwise place them after user `.bss`, undoing the anchoring. */
  _bss_radio_start = ABSOLUTE(.);
  *libbtdm_app.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libcoexist.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libcore.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libespnow.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libmesh.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libnet80211.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libphy.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libpp.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libprintf.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libregulatory.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *librtc.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libsmartconfig.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libwapi.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  *libwpa_supplicant.a:*(.sbss .sbss.* .bss .bss.* COMMON)
  . = ALIGN(16);
  _bss_radio_end = ABSOLUTE(.);

  *(.dynsbss)
  *(.sbss)
  *(.sbss.*)
  *(.gnu.linkonce.sb.*)
  *(.scommon)
  *(.sbss2)
  *(.sbss2.*)
  *(.gnu.linkonce.sb2.*)
  *(.dynbss)
  *(.sbss .sbss.* .bss .bss.*);
  *(.share.mem)
  *(.gnu.linkonce.b.*)
  *(COMMON)

  _bss_end = ABSOLUTE(.);
  . = ALIGN(4);
} > RWDATA

.noinit (NOLOAD) : ALIGN(4)
{
  . = ALIGN(4);
  *(.noinit .noinit.*)
  *(.uninit .uninit.*)
  . = ALIGN(4);
} > RWDATA
