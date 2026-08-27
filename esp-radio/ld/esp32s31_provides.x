/* Derived from the symbols the ESP32-S31 blobs actually leave undefined:
   of 2476 undefined symbols, 348 are not satisfied within the archive set,
   and exactly these four have an esp-radio implementation to alias to.
   The chip needs neither the libc shims (strdup/strrchr/gettimeofday/...)
   nor the misc_nvs_* entry points the older chips do. */
EXTERN( __ESP_RADIO_WIFI_EVENT );
EXTERN( __ESP_RADIO_G_MISC_NVS );
EXTERN( __esp_radio_putchar );
EXTERN( __esp_radio_puts );

PROVIDE( WIFI_EVENT = __ESP_RADIO_WIFI_EVENT );
PROVIDE( g_misc_nvs = __ESP_RADIO_G_MISC_NVS );
PROVIDE( putchar = __esp_radio_putchar );
PROVIDE( puts = __esp_radio_puts );
