/* Derived from the symbols the ESP32-S31 blobs actually leave undefined.
   The first cut of this file listed four entries, because it was derived from
   the blob set as Espressif ships it — which is missing `wpa_supplicant`,
   `regulatory` and `printf`. With those built from ESP-IDF v6.1 and linked,
   the supplicant pulls the rest of the usual set. */
EXTERN( __esp_radio_strdup );
EXTERN( __ESP_RADIO_G_WIFI_OSI_FUNCS );
EXTERN( __ESP_RADIO_G_WIFI_FEATURE_CAPS );
EXTERN( __ESP_RADIO_WIFI_EVENT );
EXTERN( __ESP_RADIO_G_MISC_NVS );
EXTERN( __esp_radio_gettimeofday );
EXTERN( __esp_radio_esp_fill_random );
EXTERN( __esp_radio_strrchr );
EXTERN( __esp_radio_putchar );
EXTERN( __esp_radio_puts );
EXTERN( __esp_radio_esp_timer_get_time );
EXTERN( __esp_radio_vTaskDelay );
EXTERN( __esp_radio_sleep );
EXTERN( __esp_radio_usleep );

PROVIDE( strdup = __esp_radio_strdup );
PROVIDE( g_wifi_osi_funcs = __ESP_RADIO_G_WIFI_OSI_FUNCS );
PROVIDE( g_wifi_feature_caps = __ESP_RADIO_G_WIFI_FEATURE_CAPS );
PROVIDE( WIFI_EVENT = __ESP_RADIO_WIFI_EVENT );
PROVIDE( g_misc_nvs = __ESP_RADIO_G_MISC_NVS );
PROVIDE( gettimeofday = __esp_radio_gettimeofday );
PROVIDE( esp_fill_random = __esp_radio_esp_fill_random );
PROVIDE( strrchr = __esp_radio_strrchr );
PROVIDE( putchar = __esp_radio_putchar );
PROVIDE( puts = __esp_radio_puts );
PROVIDE( esp_timer_get_time = __esp_radio_esp_timer_get_time );
PROVIDE( vTaskDelay = __esp_radio_vTaskDelay );
PROVIDE( sleep = __esp_radio_sleep );
PROVIDE( usleep = __esp_radio_usleep );

#IF wifi
EXTERN( __ESP_RADIO_G_LOG_LEVEL );
PROVIDE( g_log_level = __ESP_RADIO_G_LOG_LEVEL );
#ENDIF
