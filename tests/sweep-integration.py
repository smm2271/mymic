"""Real localhost UDP / control / API integration; no Wi-Fi or acoustic claim."""
import json, os, subprocess, time, urllib.request, urllib.error
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/os.environ.get('MIC_TEST_BIN','target/debug')
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
    env=os.environ.copy();env.update(MIC_HTTP_PORT='18380',MIC_DISCOVERY_PORT='41300',MIC_CONTROL_PORT='41301',MIC_AUDIO_PORT='41302',MIC_LAB_DIR=str(ROOT/'artifacts/sweep-store'))
    launch('mic-server',env=env);wait(lambda:request('/status')[0]==200);TOKEN=request('/status')[1]['api_token']
    code='0123456789abcdef0123456789abcdef'
    launch('mic-simulator','--id','lab-tx','--discovery','127.0.0.1:41300','--code',code,'--loss-every','13','--timing-mode','waitable')
    wait(lambda:request('/devices')[1]);assert request('/devices/lab-tx/pair','POST',{'code':code})[0]==200
    assert request('/devices/lab-tx/audio','PATCH',{'streaming':True,'gain':.1})[0]==200
    wait(lambda:request('/metrics')[1]['received_packets']>20)
    matrix={'frame':[5,10],'fec':[False,True],'jitter':[20,30,50]}
    status,cs=request('/lab/sweep/preview','POST',matrix);assert status==200 and len(cs)==12
    for c in cs:c['enabled']=False
    cs[0]['enabled']=True
    plan=dict(plan_id='integration',name='UDP sweep',created_at=0,device_id='lab-tx',matrix_parameters=matrix,selected_combinations=cs,global_settings=dict(duration_seconds=10,warmup_seconds=0,repetitions=2,mode='Step-by-step',order='Randomized'),execution_order=[],random_seed=42)
    assert request('/lab/sweep/plan','PUT',plan)[0]==200
    assert request('/lab/sweep/plan')[1]['plan_id']=='integration'
    assert request('/lab/sweep','POST',plan)[0]==200
    assert request('/devices/lab-tx/audio','PATCH',{'gain':.2})[0]==409
    assert request('/lab/sweep','POST',plan)[0]==409
    deadline=time.monotonic()+20
    while time.monotonic()<deadline:
        result=request('/lab/sweep')[1]
        if result['status']=='WAITING_FOR_USER':break
        time.sleep(.1)
    assert result['status']=='WAITING_FOR_USER',result
    assert result['runs'][0]['validity']=='VALID',result['runs'][0]
    assert result['runs'][0]['duration_seconds']>=10
    assert request('/lab/sweep/continue','POST',{})[0]==200
    wait(lambda:request('/lab/sweep')[1]['status']=='MEASURING')
    assert request('/lab/sweep/cancel','POST',{})[0]==200
    wait(lambda:request('/lab/sweep')[1]['status']=='CANCELLED')
    result=request('/lab/sweep')[1]
    assert len(result['runs'])==2 and result['runs'][1]['validity']=='INTERRUPTED'
    assert result['comparison_summary'][0]['valid_runs']==1
    assert request('/lab/sweep/export')[1]['schema_version']==2
    (ROOT/'artifacts/sweep-integration.json').write_text(json.dumps(result,indent=2),encoding='utf8')
    with urllib.request.urlopen(BASE+'/lab/sweep/export?format=csv') as r:
        assert 'underruns_per_second_mean' in r.read().decode()
    suite_id=result['suite_id']
    assert suite_id in request('/lab/sweep/history')[1]
    assert request('/lab/sweep/history/'+suite_id)[1]['runs']==result['runs']
    server,log=processes[0];server.terminate();server.wait(timeout=5)
    launch('mic-server',env=env);wait(lambda:request('/status')[0]==200)
    TOKEN=request('/status')[1]['api_token']
    restored=request('/lab/sweep')[1]
    assert restored['runs']==result['runs'] and restored['status']=='CANCELLED'
    print('PASS matrix, saved plan, real signed UDP ACK, backend timing, step mode, mutation guard, cancellation, comparison and JSON')

finally:
    for p,log in reversed(processes):
        p.terminate()
        try:p.wait(timeout=5)
        except subprocess.TimeoutExpired:p.kill();p.wait()
        log.close()
