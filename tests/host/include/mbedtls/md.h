/* Windows host-test adapter; firmware uses the ESP-IDF mbedTLS implementation. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#define MBEDTLS_MD_SHA256 6
typedef int mbedtls_md_info_t;
const mbedtls_md_info_t *mbedtls_md_info_from_type(int type);
int mbedtls_md(const mbedtls_md_info_t *info,const uint8_t *input,size_t len,uint8_t out[32]);
int mbedtls_md_hmac(const mbedtls_md_info_t *info,const uint8_t *key,size_t key_len,const uint8_t *input,size_t len,uint8_t out[32]);
