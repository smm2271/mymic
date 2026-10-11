"""Independent Python encoder; all key material is a public test fixture."""
import hashlib, hmac, json, struct
from pathlib import Path
key=bytes(range(32)); session=bytes(range(16)); vectors=[]
for samples,kind in [(80,1),(160,1),(80,2)]:
    seq=0xfffffffc; stamp=(seq*samples)&0xffffffff
    pcm=[((i*333+123)&0xffff)-32768 for i in range(samples)]
    body=b'WMIC'+bytes([2,kind])+struct.pack('<IIH',seq,stamp,samples*2)+session+struct.pack('<I',42)+struct.pack('<'+'h'*samples,*pcm)
    vectors.append(dict(samples=samples,kind=kind,sequence=seq,timestamp=stamp,pcm=pcm,packet_hex=(body+hmac.digest(key,body,'sha256')[:16]).hex()))
canonical=f'1|SET_TRANSPORT|42|golden-device||{session.hex()}|0|0||||lab1|5|1|xor|4|1|40|0'
out=dict(key_hex=key.hex(),session_hex=session.hex(),packets=vectors,control_canonical=canonical,control_proof=hmac.digest(key,canonical.encode(),'sha256').hex())
Path(__file__).with_name('lab-vectors.json').write_text(json.dumps(out,indent=2)+'\n',encoding='utf8')
