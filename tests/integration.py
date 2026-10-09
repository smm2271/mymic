"""Black-box API/UDP integration; uses built binaries and the production protocol."""
import json, os, socket, subprocess, time, urllib.request, urllib.error, sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BASE='http://127.0.0.1:18080/api/v1'
CODE='0123456789abcdef0123456789abcdef' # public test fixture, never production credential
processes=[]
checks=[]
def check(condition,name):
 if not condition: raise AssertionError(name)
 checks.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=8):
 end=time.monotonic()+seconds
 while time.monotonic()<end:
  try:
   value=fn()
   if value:return value
  except (OSError,KeyError,IndexError,StopIteration):pass
  time.sleep(.1)
 raise AssertionError('condition timed out')
def request(path,method='GET',body=None,token=True,origin=None):
 headers={'Content-Type':'application/json'}
 if token:headers['X-Mic-Token']=TOKEN
 if origin:headers['Origin']=origin
 r=urllib.request.Request(BASE+path,data=json.dumps(body).encode() if body is not None else None,headers=headers,method=method)
 try:
  with urllib.request.urlopen(r,timeout=6) as response:return response.status,json.load(response)
 except urllib.error.HTTPError as e:return e.code,json.load(e)
def launch(name,*args,env=None):
 f=open(ROOT/'artifacts'/f'{name}-{len(processes)}.log','w',encoding='utf8');p=subprocess.Popen([str(ROOT/'target'/os.environ.get('MIC_TEST_PROFILE','debug')/f'{name}.exe'),*args],cwd=ROOT,stdout=f,stderr=f,env=env);processes.append((p,f));return p
def device(id='sim-test'):
 return next(d for d in request('/devices')[1] if d['id']==id)
def state(value,id='sim-test'):return device(id)['state']==value
TOKEN=''
try:
 (ROOT/'artifacts').mkdir(exist_ok=True)
 env=os.environ.copy();env.update(MIC_HTTP_PORT='18080',MIC_DISCOVERY_PORT='41100',MIC_CONTROL_PORT='41101',MIC_AUDIO_PORT='41102')
 server=launch('mic-server',env=env)
 wait(lambda:request('/status')[0]==200)
 TOKEN=request('/status')[1]['api_token']
 check(request('/status')[1]['service']=='online','service starts independently of UI')
 outputs=request('/audio/outputs')[1];check(isinstance(outputs,list),'CPAL enumerates playback outputs');print('OUTPUTS',json.dumps(outputs,ensure_ascii=False),flush=True)
 check(request('/audio/output','PUT',{'id':'default'},token=False)[0]==403,'mutation rejects missing token')
 check(request('/status',origin='https://malicious.example')[0]==403,'hostile browser Origin rejected')
 check(request('/events',origin='https://malicious.example')[0]==403,'hostile WebSocket Origin rejected')
 subprocess.run(['node',str(ROOT/'tests/websocket.mjs'),'http://127.0.0.1:18080'],check=True,timeout=8)
 check(True,'WebSocket snapshot delivery')
 check(request('/devices/x/audio','PATCH',{'gain':1.5})[0]==400,'API gain validation')
 check(request('/devices/x/audio','PATCH',{'source':'unknown'})[0]==400,'API source validation')
 check(request('/devices/x/audio','PATCH',{'extra':True})[0]==400,'API unknown fields rejected')
 sim=launch('mic-simulator','--id','sim-test','--discovery','127.0.0.1:41100','--code',CODE,'--loss-every','13','--reorder-every','7','--jitter-ms','4','--reconnect-after','16')
 wait(lambda:state('DISCOVERED'));check(True,'discovery populates registry')
 check(request('/devices/sim-test/pair','POST',{'code':'f'*32})[0]==400,'incorrect credential rejected')
 check(request('/devices/unknown/disconnect','POST',{})[0]==404,'missing device returns 404')
 check(request('/devices/sim-test/pair','POST',{'code':CODE})[0]==200,'credential pairing succeeds')
 wait(lambda:state('CONNECTED'));check(True,'connected is distinct from streaming')
 check(request('/devices/sim-test/audio','PATCH',{'streaming':True})[0]==200,'START_STREAM is acknowledged')
 wait(lambda:state('STREAMING'));wait(lambda:request('/metrics')[1]['received_packets']>100)
 check(request('/devices/sim-test/audio','PATCH',{'source':'sweep','gain':.25,'mute':True})[0]==200,'source/gain/mute commands acknowledged')
 wait(lambda:device()['source']=='sweep' and device()['mute'] and device()['gain']==.25)
 check(True,'settings update from confirmed device state')
 check(request('/devices/sim-test/audio','PATCH',{'source':'sine','mute':False})[0]==200,'switch back to sine')
 metrics=request('/metrics')[1];check(metrics['lost_packets']>0,'packet loss detected');check(metrics['out_of_order_packets']>0,'packet reordering detected');check(metrics['jitter_buffer_depth']<=8,'jitter storage remains bounded')
 check(request('/audio/output','PUT',{'id':'default'})[0]==200,'Windows default output stream opens')
 if outputs:check(request('/audio/output','PUT',{'id':outputs[0]['id']})[0]==200,'output device switching')
 # No browser/UI is running throughout this check.
 before=request('/metrics')[1]['received_packets'];time.sleep(1.1);check(request('/metrics')[1]['received_packets']>before+50,'stream persists without Web UI')
 # Simulator changes discovery nonce at 16 s; observe session loss and authenticated auto reconnect.
 wait(lambda:state('STREAMING'),25)
 wait(lambda:'Simulated reconnect' in (ROOT/'artifacts/mic-simulator-1.log').read_text(encoding='utf8'),22)
 wait(lambda:(ROOT/'artifacts/mic-simulator-1.log').read_text(encoding='utf8').count('Paired session')>=2,12)
 wait(lambda:state('STREAMING'),12);check(device()['source']=='sine' and device()['gain']==.25 and not device()['mute'],'reconnect restores confirmed settings');before=request('/metrics')[1]['received_packets'];time.sleep(.8);check(request('/metrics')[1]['received_packets']>before+30,'fresh-nonce automatic reconnect resumes streaming')
 check(request('/devices/sim-test/audio','PATCH',{'streaming':False})[0]==200,'STOP_STREAM acknowledged');wait(lambda:state('CONNECTED'));before=request('/metrics')[1]['received_packets'];time.sleep(.8);check(request('/metrics')[1]['received_packets']<=before+2,'stopped stream does not send audio')
 check(request('/devices/sim-test/disconnect','POST',{})[0]==200,'explicit disconnect');wait(lambda:state('OFFLINE'));check(True,'disconnect clears approval')
 # Valid discover, silent control peer exercises retry/timeout through real UDP.
 import hashlib
 fake=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);fake.bind(('127.0.0.1',0));nonce='a'*32
 m={'version':1,'kind':'DISCOVER','request_id':1,'device_id':'silent-device','name':'Timeout fixture','device_type':'test','capabilities':[],'nonce':nonce,'commitment':hashlib.sha256((CODE+'|'+nonce).encode()).hexdigest(),'control_port':fake.getsockname()[1]}
 fake.sendto(json.dumps(m).encode(),('127.0.0.1',41100));wait(lambda:state('DISCOVERED','silent-device'))
 code,result=request('/devices/silent-device/pair','POST',{'code':CODE});check(code==504 and 'timeout' in result['error']['message'],'pairing times out after bounded retries');wait(lambda:state('DISCOVERED','silent-device'));check(True,'pairing timeout returns to discovered')
 check(request('/devices/silent-device/pair','POST',{'code':'','reject':True})[0]==200,'user rejection')
 fake.sendto(json.dumps(m).encode(),('127.0.0.1',41100));time.sleep(.4);check(state('OFFLINE','silent-device'),'rejected nonce remains suppressed');fake.close()
 print(json.dumps({'checks':checks,'metrics':request('/metrics')[1]},ensure_ascii=False,indent=2),flush=True)
 (ROOT/'artifacts/integration-result.json').write_text(json.dumps({'checks':checks,'metrics':request('/metrics')[1]},ensure_ascii=False,indent=2),encoding='utf8')
finally:
 for p,f in reversed(processes):
  if p.poll() is None:p.terminate()
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:p.kill();p.wait()
  f.close()
