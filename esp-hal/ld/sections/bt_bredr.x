/* Classic-BT exchange memory, usable by a BLE-only application. NOLOAD: the
   controller blob owns these addresses in a Classic BT build, so nothing may
   be initialised here at load time. See `bt_bredr_seg` in esp32/memory.x. */
SECTIONS {
    .bt_bredr_uninit (NOLOAD) : ALIGN(4) {
        *(.bt_bredr_uninit)
    } > bt_bredr_seg
}
