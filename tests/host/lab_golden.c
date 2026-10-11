#include <assert.h>
#include <stdio.h>
#include "protocol.h"
void lab_golden(const char *path) {
 FILE *fp=fopen(path,"rb");assert(fp);char json[16384];size_t size=fread(json,1,sizeof(json)-1,fp);fclose(fp);json[size]=0;cJSON *v=cJSON_Parse(json);assert(v);
 uint8_t key[32],session[16],expected[372],actual[372],digest[32];assert(!mic_unhex(mic_str(v,"key_hex"),32,key));assert(!mic_unhex(mic_str(v,"session_hex"),16,session));
 cJSON *packets=cJSON_GetObjectItemCaseSensitive(v,"packets");
 for(int n=0;n<cJSON_GetArraySize(packets);n++){cJSON *p=cJSON_GetArrayItem(packets,n);unsigned count=mic_num(p,"samples");int16_t pcm[160]={0};cJSON *a=cJSON_GetObjectItemCaseSensitive(p,"pcm");for(unsigned i=0;i<count;i++)pcm[i]=(int16_t)cJSON_GetArrayItem(a,i)->valueint;
 unsigned body=mic_encode_lab(actual,(uint32_t)cJSON_GetObjectItemCaseSensitive(p,"sequence")->valuedouble,(uint32_t)cJSON_GetObjectItemCaseSensitive(p,"timestamp")->valuedouble,session,pcm,count,42,mic_num(p,"kind")==2);mic_hmac(key,32,actual,body,digest);memcpy(actual+body,digest,16);assert(!mic_unhex(mic_str(p,"packet_hex"),body+16,expected));assert(!memcmp(actual,expected,body+16));}
 cJSON *m=mic_message("SET_TRANSPORT","golden-device",42);cJSON_AddStringToObject(m,"session",mic_str(v,"session_hex"));cJSON *t=cJSON_AddObjectToObject(m,"transport");cJSON_AddNumberToObject(t,"frame_duration_ms",5);cJSON_AddNumberToObject(t,"jitter_buffer_target_ms",40);cJSON *f=cJSON_AddObjectToObject(t,"fec");cJSON_AddBoolToObject(f,"enabled",1);cJSON_AddStringToObject(f,"scheme","xor");cJSON_AddNumberToObject(f,"data_shards",4);cJSON_AddNumberToObject(f,"parity_shards",1);cJSON_AddNumberToObject(m,"transport_stream",0);
 mic_sign(m,key,32);assert(!strcmp(mic_str(m,"proof"),mic_str(v,"control_proof")));assert(mic_verify(m,key,32));cJSON_ReplaceItemInObject(t,"frame_duration_ms",cJSON_CreateNumber(5.5));assert(!mic_verify(m,key,32));cJSON_Delete(m);cJSON_Delete(v);puts("PASS Lab Python/C/Rust 5/10 ms, parity, authenticated settings vectors");
}
