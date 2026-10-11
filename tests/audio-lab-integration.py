"""Real localhost UDP / control / API integration; no Wi-Fi or acoustic claim."""
import json, os, subprocess, time, urllib.request, urllib.error
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/os.environ.get('MIC_TEST_BIN','target/drift-validation/debug')
BASE='http://127.0.0.1:18380/api/v1'; TOKEN=''; processes=[]
def request(path,method='GET',body=None,token=True):
    headers={'Content-Type':'application/json'}
    if token: headers['X-Mic-Token']=TOKEN
    req=urllib.request.Request(BASE+path,headers=headers,method=method,data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req,timeout=8) as r:return r.status,json.load(r)
    except urllib.error.HTTPError as e:return e.code,json.load(e)
def wait(fn):
    end=time.monotonic()+10
    while time.monotonic()<end:
        try:
            value=fn()
            if value:return value
        except (OSError,KeyError,IndexError):pass
        time.sleep(.1)
    raise AssertionError('condition timed out')
def launch(name,*args,env=None):
    log=open(ROOT/'artifacts'/f'lab-{name}.log','w',encoding='utf8')
    p=subprocess.Popen([str(BIN/f'{name}.exe'),*args],cwd=ROOT,stdout=log,stderr=log,env=env);processes.append((p,log))
def config(ms,fec,target):return dict(frame_duration_ms=ms,fec=dict(enabled=fec,scheme='xor',data_shards=4,parity_shards=1),jitter_buffer_target_ms=target)
try:
    (ROOT/'artifacts').mkdir(exist_ok=True)
    env=os.environ.copy();env.update(MIC_HTTP_PORT='18380',MIC_DISCOVERY_PORT='41300',MIC_CONTROL_PORT='41301',MIC_AUDIO_PORT='41302')
    launch('mic-server',env=env);wait(lambda:request('/status')[0]==200);TOKEN=request('/status')[1]['api_token']
    code='0123456789abcdef0123456789abcdef'
    launch('mic-simulator','--id','lab-tx','--discovery','127.0.0.1:41300','--code',code,'--loss-every','13','--timing-mode','waitable')
    wait(lambda:request('/devices')[1]);assert request('/devices/lab-tx/pair','POST',{'code':code})[0]==200
    assert request('/devices/lab-tx/audio','PATCH',{'streaming':True,'gain':.1})[0]==200
    wait(lambda:request('/metrics')[1]['received_packets']>20)
    assert request('/devices/lab-tx/transport','PUT',config(5,True,40),False)[0]==403
    assert request('/devices/lab-tx/transport','PUT',config(6,True,40))[0]==400
    evidence=[]
    for ms,fec,target in [(10,False,30),(5,False,30),(5,True,30),(5,True,40),(5,True,60),(10,False,30)]:
        status,response=request('/devices/lab-tx/transport','PUT',config(ms,fec,target));assert status==200,(status,response);assert response['confirmed']==config(ms,fec,target)
        before=request('/metrics')[1];fec_before=request('/lab')[1]['fec'];time.sleep(1.3);after=request('/metrics')[1];lab=request('/lab')[1];assert lab['lab']['lab-tx']['status']=='applied';assert lab['lab']['lab-tx']['applied']==config(ms,fec,target)
        if fec:assert lab['fec']['recovered']>fec_before['recovered'] and lab['fec']['parity_received']>fec_before['parity_received']
        assert after['accepted_packets']>before['accepted_packets'];assert after['jitter_capacity_packets']==160//ms
        assert after['target_buffer_depth']==target//ms;assert after['jitter_buffer_depth']<=160//ms
        evidence.append(dict(config=config(ms,fec,target),before=before,after=after,fec_before=fec_before,fec_after=lab["fec"],applied=lab["lab"]["lab-tx"]))
        print('PASS real UDP negotiated',ms,fec,target,flush=True)
    assert request('/devices/lab-tx/audio','PATCH',{'streaming':False})[0]==200
    # Stop is a safe quiescent boundary; start creates a fresh authenticated token.
    assert request('/devices/lab-tx/audio','PATCH',{'streaming':True})[0]==200
    (ROOT/'artifacts/audio-lab-integration.json').write_text(json.dumps(evidence,indent=2),encoding='utf8')
    print('PASS token guard, invalid config, baseline, switching, physical/target bounds, stop/restart')
finally:
    for p,log in reversed(processes):
        p.terminate()
        try:p.wait(timeout=5)
        except subprocess.TimeoutExpired:p.kill();p.wait()
        log.close()
