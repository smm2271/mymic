#pragma once
#include <stdint.h>
#include <string.h>
#define MIC_PACKET_SIZE 368
static inline void mic_le32(uint8_t *p,uint32_t v){for(int i=0;i<4;i++)p[i]=(uint8_t)(v>>(8*i));}
/* Explicit byte encoding: no packed/native C structs on the wire. Tag appended separately. */
static inline void mic_encode_body(uint8_t out[MIC_PACKET_SIZE],uint32_t seq,uint32_t timestamp,const uint8_t session[16],const int16_t samples[160]){
 memcpy(out,"WMIC",4);out[4]=1;out[5]=1;mic_le32(out+6,seq);mic_le32(out+10,timestamp);out[14]=0x40;out[15]=0x01;memcpy(out+16,session,16);
 for(int i=0;i<160;i++){uint16_t v=(uint16_t)samples[i];out[32+2*i]=(uint8_t)v;out[33+2*i]=(uint8_t)(v>>8);}
}
static inline int mic_decode_body(const uint8_t *in,unsigned n,uint32_t *seq,uint32_t *timestamp,int16_t samples[160]) {
 if(n!=MIC_PACKET_SIZE||memcmp(in,"WMIC",4)||in[4]!=1||in[5]!=1||in[14]!=0x40||in[15]!=1)return -1;
 *seq=0;*timestamp=0;for(int i=0;i<4;i++){*seq|=(uint32_t)in[6+i]<<(8*i);*timestamp|=(uint32_t)in[10+i]<<(8*i);}for(int i=0;i<160;i++)samples[i]=(int16_t)((uint16_t)in[32+i*2]|(uint16_t)in[33+i*2]<<8);return 0;
}
