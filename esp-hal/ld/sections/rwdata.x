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

.bss (NOLOAD) : ALIGN(4)
{
  _bss_start = ABSOLUTE(.);
  . = ALIGN (4);

  /* ESP32 BLE/PHY blobs need stable addresses across user-`.bss` growth.
     Pin them first inside `.bss`. Other chips have no such blobs here;
     leaving the rules ungated shifted `.bss` layout on every target. */
#IF esp32
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
  . = ALIGN(4);
  _bss_radio_end = ABSOLUTE(.);
#ENDIF

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
