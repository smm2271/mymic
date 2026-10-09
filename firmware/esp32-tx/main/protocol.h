#pragma once
#include "cJSON.h"
#include "packet.h"
void mic_hex(const uint8_t *in,int n,char *out);
int mic_unhex(const char *in,int n,uint8_t *out);
void mic_hmac(const uint8_t *key,int key_len,const uint8_t *data,int len,uint8_t out[32]);
void mic_commitment(const char *code,const char *nonce,char out[65]);
const char *mic_str(const cJSON *m,const char *key);
int mic_num(const cJSON *m,const char *key);
cJSON *mic_message(const char *kind,const char *id,uint32_t request);
void mic_sign(cJSON *m,const uint8_t *key,int key_len);
int mic_verify(cJSON *m,const uint8_t *key,int key_len);
