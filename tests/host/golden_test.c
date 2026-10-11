#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include "protocol.h"
#include "audio_source.h"
void lab_golden(const char *path);
int main(int argc,char **argv){assert(argc==2||argc==3);if(argc==3)lab_golden(argv[2]);FILE *f=fopen(argv[1],"rb");assert(f);char json[4096];size_t n=fread(json,1,sizeof(json)-1,f);fclose(f);json[n]=0;cJSON *v=cJSON_Parse(json);assert(v);
 uint8_t key[32],session[16],expected[368],actual[368],digest[32];assert(!mic_unhex(mic_str(v,"key_hex"),32,key));assert(!mic_unhex(mic_str(v,"session_hex"),16,session));assert(!mic_unhex(mic_str(v,"packet_hex"),368,expected));int16_t samples[160]={-32768,-1,0,1,32767};mic_encode_body(actual,UINT32_MAX,160,session,samples);mic_hmac(key,32,actual,352,digest);memcpy(actual+352,digest,16);assert(!memcmp(actual,expected,368));uint32_t seq,ts;int16_t decoded[160];assert(!mic_decode_body(actual,368,&seq,&ts,decoded));assert(seq==UINT32_MAX&&ts==160&&!memcmp(samples,decoded,sizeof samples));assert(mic_decode_body(actual,367,&seq,&ts,decoded));actual[4]=99;assert(mic_decode_body(actual,368,&seq,&ts,decoded));
 cJSON *m=mic_message("SET_GAIN","golden-device",UINT32_MAX);cJSON_AddStringToObject(m,"session",mic_str(v,"session_hex"));cJSON_AddNumberToObject(m,"gain",0.25);mic_sign(m,key,32);assert(!strcmp(mic_str(m,"proof"),mic_str(v,"control_proof")));assert(mic_verify(m,key,32));cJSON_ReplaceItemInObject(m,"gain",cJSON_CreateNumber(.9));assert(!mic_verify(m,key,32));cJSON_Delete(m);
 audio_source_t source={.kind=AUDIO_SINE};audio_source_frame(&source,1,0,samples);int energy=0;for(int i=0;i<160;i++){assert(abs(samples[i])<=4916);energy+=abs(samples[i]);}assert(energy>0);source.kind=AUDIO_SILENCE;audio_source_frame(&source,1,0,samples);for(int i=0;i<160;i++)assert(samples[i]==0);source.kind=AUDIO_SWEEP;audio_source_frame(&source,1,1,samples);for(int i=0;i<160;i++)assert(samples[i]==0);
 cJSON_Delete(v);puts("PASS C/Rust audio and control golden vectors, invalid headers, synthetic audio safety");return 0;
}
