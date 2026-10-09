idf_component_register(SRCS "main.c" "protocol.c" "audio_source.c" INCLUDE_DIRS "." REQUIRES esp_wifi esp_event esp_netif nvs_flash json mbedtls esp_timer)
