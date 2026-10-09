#include <windows.h>
#include <bcrypt.h>
#include "mbedtls/md.h"
static int info=MBEDTLS_MD_SHA256;
const mbedtls_md_info_t *mbedtls_md_info_from_type(int type){return type==MBEDTLS_MD_SHA256?&info:NULL;}
static int digest(const uint8_t *key,size_t key_len,const uint8_t *data,size_t len,uint8_t out[32],int hmac){
 BCRYPT_ALG_HANDLE algorithm=NULL;BCRYPT_HASH_HANDLE hash=NULL;int result=-1;
 if(BCryptOpenAlgorithmProvider(&algorithm,BCRYPT_SHA256_ALGORITHM,NULL,hmac?BCRYPT_ALG_HANDLE_HMAC_FLAG:0))goto done;
 if(BCryptCreateHash(algorithm,&hash,NULL,0,(PUCHAR)key,(ULONG)key_len,0))goto done;
 if(BCryptHashData(hash,(PUCHAR)data,(ULONG)len,0))goto done;
 if(BCryptFinishHash(hash,out,32,0))goto done;
 result=0;
 done:if(hash)BCryptDestroyHash(hash);if(algorithm)BCryptCloseAlgorithmProvider(algorithm,0);return result;
}
int mbedtls_md(const mbedtls_md_info_t *i,const uint8_t *b,size_t n,uint8_t out[32]){(void)i;return digest(NULL,0,b,n,out,0);}
int mbedtls_md_hmac(const mbedtls_md_info_t *i,const uint8_t *key,size_t k,const uint8_t *b,size_t n,uint8_t out[32]){(void)i;return digest(key,k,b,n,out,1);}
