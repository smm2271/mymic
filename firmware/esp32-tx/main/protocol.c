#include "protocol.h"
#include "mbedtls/md.h"
#include <stdio.h>
#include <stdlib.h>
#include <math.h>
void mic_hex(const uint8_t *in,int n,char *out){const char *hex="0123456789abcdef";for(int i=0;i<n;i++){out[i*2]=hex[in[i]>>4];out[i*2+1]=hex[in[i]&15];}out[2*n]=0;}
static int digit(char c){if(c>='0'&&c<='9')return c-'0';if(c>='a'&&c<='f')return c-'a'+10;return -1;}
int mic_unhex(const char *s,int n,uint8_t *out){if(strlen(s)!=(unsigned)n*2)return -1;for(int i=0;i<n;i++){int a=digit(s[2*i]),b=digit(s[2*i+1]);if(a<0||b<0)return -1;out[i]=(uint8_t)(a*16+b);}return 0;}
void mic_hmac(const uint8_t *key,int key_len,const uint8_t *data,int len,uint8_t out[32]){mbedtls_md_hmac(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),key,key_len,data,len,out);}
void mic_commitment(const char *code,const char *nonce,char out[65]){char b[80];snprintf(b,sizeof b,"%s|%s",code,nonce);uint8_t digest[32];mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),(uint8_t*)b,strlen(b),digest);mic_hex(digest,32,out);}
const char *mic_str(const cJSON *m,const char *key){cJSON *v=cJSON_GetObjectItemCaseSensitive(m,key);return cJSON_IsString(v)?v->valuestring:"";}
int mic_num(const cJSON *m,const char *key){cJSON *v=cJSON_GetObjectItemCaseSensitive(m,key);return cJSON_IsNumber(v)?(int)v->valuedouble:0;}
cJSON *mic_message(const char *kind,const char *id,uint32_t request){cJSON *m=cJSON_CreateObject();cJSON_AddNumberToObject(m,"version",1);cJSON_AddStringToObject(m,"kind",kind);cJSON_AddNumberToObject(m,"request_id",request);cJSON_AddStringToObject(m,"device_id",id);return m;}
static int signing(cJSON *m,char out[512]){char gain[16]="",mute[2]="";cJSON *g=cJSON_GetObjectItemCaseSensitive(m,"gain"),*v=cJSON_GetObjectItemCaseSensitive(m,"mute");if(cJSON_IsNumber(g))snprintf(gain,sizeof gain,"%u",(unsigned)lround(g->valuedouble*1000.0));if(cJSON_IsBool(v)){mute[0]=cJSON_IsTrue(v)?'1':'0';}
 return snprintf(out,512,"1|%s|%u|%s|%s|%s|%u|%u|%s|%s|%s",mic_str(m,"kind"),(unsigned)cJSON_GetObjectItemCaseSensitive(m,"request_id")->valuedouble,mic_str(m,"device_id"),mic_str(m,"nonce"),mic_str(m,"session"),(unsigned)mic_num(m,"audio_port"),(unsigned)mic_num(m,"control_port"),mic_str(m,"source"),gain,mute);}
void mic_sign(cJSON *m,const uint8_t *key,int key_len){char bytes[512],proof[65];uint8_t digest[32];int n=signing(m,bytes);mic_hmac(key,key_len,(uint8_t*)bytes,n,digest);mic_hex(digest,32,proof);cJSON_DeleteItemFromObjectCaseSensitive(m,"proof");cJSON_AddStringToObject(m,"proof",proof);}
int mic_verify(cJSON *m,const uint8_t *key,int key_len){char bytes[512];uint8_t actual[32],expected[32];if(mic_unhex(mic_str(m,"proof"),32,actual))return 0;int n=signing(m,bytes);if(n<0||n>=512)return 0;mic_hmac(key,key_len,(uint8_t*)bytes,n,expected);unsigned difference=0;for(int i=0;i<32;i++)difference|=actual[i]^expected[i];return difference==0;}
