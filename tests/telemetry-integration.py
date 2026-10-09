"""v0.5 production binary integration. Fixture credentials never authorize hardware."""
import json,os,subprocess,time,urllib.request,urllib.error
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1];BASE='http://127.0.0.1:18081/api/v1';CODE='0123456789abcdef0123456789abcdef';TOKEN='';processes=[];checks=[]
def check(ok,name):
 assert ok,name
 checks.append(name);print('PASS',name,flush=True)
def request(path,method='GET',body=None):
 req=urllib.request.Request(BASE+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json','X-Mic-Token':TOKEN})
 try:
  with urllib.request.urlopen(req,timeout=6) as r:return r.status,json.load(r)
 except urllib.error.HTTPError as e:return e.code,json.load(e)
def wait(fn,timeout=10):
 end=time.monotonic()+timeout
 while time.monotonic()<end:
  try:
   result=fn()
   if result:return result
  except (OSError,StopIteration,KeyError,IndexError):pass
  time.sleep(.1)
 raise AssertionError('wait timed out')
def launch(name,*args,env=None):
 f=open(ROOT/'artifacts'/f'telemetry-{name}-{len(processes)}.log','w',encoding='utf8');p=subprocess.Popen([str(ROOT/'target/release'/f'{name}.exe'),*args],cwd=ROOT,stdout=f,stderr=f,env=env);processes.append((p,f));return p

def history(id='tx-a',range='15m'):return request(f'/telemetry/history?device_id={id}&range={range}')[1]
def state(id):return next(d['state'] for d in request('/devices')[1] if d['id']==id)
def pair_start(id):
 check(request(f'/devices/{id}/pair','POST',{'code':CODE})[0]==200,f'{id} pair')
 check(request(f'/devices/{id}/audio','PATCH',{'streaming':True,'mute':True})[0]==200,f'{id} start muted')
 wait(lambda:state(id)=='STREAMING')
try:
 (ROOT/'artifacts').mkdir(exist_ok=True)
 env=os.environ.copy();env.update(MIC_HTTP_PORT='18081',MIC_DISCOVERY_PORT='41200',MIC_CONTROL_PORT='41201',MIC_AUDIO_PORT='41202')
 launch('mic-server',env=env);wait(lambda:request('/status')[0]==200);TOKEN=request('/status')[1]['api_token']
 for path in ['/telemetry/history','/telemetry/history?device_id=x&range=1h','/telemetry/history?device_id=..%2Fx&range=60s','/telemetry/history?device_id=x&range=60s&extra=1','/telemetry/history?device_id=x&device_id=y&range=60s']:
  status,error=request(path);check(status==400 and 'error' in error,f'query rejection {path}')
 check(request('/telemetry/history?device_id=missing&range=60s')[0]==404,'unknown device 404')
 launch('mic-simulator','--id','tx-a','--discovery','127.0.0.1:41200','--code',CODE,'--reconnect-after','16','--loss-every','11')
 launch('mic-simulator','--id','tx-b','--discovery','127.0.0.1:41200','--code',CODE)
 wait(lambda:state('tx-a')=='DISCOVERED' and state('tx-b')=='DISCOVERED');wait(lambda:len(history()['snapshots'])>=2)
 check(all(s['receive_bitrate'] is None for s in history()['snapshots']),'discovered device has no fake rate')
 pair_start('tx-a');wait(lambda:len([s for s in history()['snapshots'] if s['received_packets_delta'] is not None])>=3)
 valid=[s for s in history()['snapshots'] if s['received_packets_delta'] is not None]
 check(all(500<=s['interval_ms']<=1500 and 0<=s['packet_loss_rate']<=100 for s in valid),'real per-second intervals and finite loss')
 wait(lambda:any(s.get('tx') is not None for s in history()['snapshots']))
 measured=next(s for s in reversed(history()['snapshots']) if s.get('tx') is not None)
 check(measured['tx']['successfully_sent_packets']>0 and measured['tx']['intentional_loss_drops']>0 and measured['tx_packet_rate'] is not None,'signed real TX timing survives history serialization')
 check(measured['rx_packet_rate'] is not None and measured['loss_basis']=='playout_deadline_deficit' and measured['bitrate_basis']=='pcm_payload','rates expose correct deficit and payload basis')
 check(all(s['cumulative']['received_packets']==0 and s['received_packets_delta'] is None for s in history('tx-b')['snapshots']),'idle TX isolated from active TX')
 subprocess.run(['node','--experimental-strip-types',str(ROOT/'tests/telemetry-websocket.mjs'),'http://127.0.0.1:18081','tx-a',str(ROOT/'artifacts/telemetry-websocket-result.json')],check=True,timeout=12)
 check(True,'actual WebSocket reconnect without repeats')
 before=history()['snapshots'][-1]['sequence'];packets=history()['snapshots'][-1]['cumulative']['received_packets'];time.sleep(2.1)
 check(history()['snapshots'][-1]['sequence']>before,'core samples with Web UI closed')
 check(history()['snapshots'][-1]['cumulative']['received_packets']>packets+100,'audio continues during telemetry and history access')
 # Simulator reboot-like fresh nonce is configured at t=16 seconds.
 wait(lambda:sum(e['event_type']=='SESSION_CHANGED' for e in history()['events'])>=2,15);wait(lambda:state('tx-a')=='STREAMING')
 wait(lambda:len({s['session_id'] for s in history()['snapshots'] if s['session_id']})>=2)
 h=history();kinds={e['event_type'] for e in h['events']};check({'START_STREAM','DISCONNECT','RECONNECT','SESSION_CHANGED'}<=kinds,'exact state transition events including reconnect')
 sessions={s['session_id'] for s in h['snapshots'] if s['session_id']};check(len(sessions)>=2,'history retains separate Session IDs')
 new_session=[e['session_id'] for e in h['events'] if e['event_type']=='SESSION_CHANGED'][-1]
 first_new_session=next(s for s in h['snapshots'] if s['session_id']==new_session)
 check(first_new_session['packet_loss_rate'] is None and first_new_session['received_packets_delta'] is None,'new session establishes null baseline without loss spike')
 check(request('/devices/tx-a/audio','PATCH',{'streaming':False})[0]==200,'stop A')
 wait(lambda:history()['snapshots'][-1]['connection_state']=='CONNECTED');time.sleep(1.2)
 stopped=history()['snapshots'][-1];check(stopped['buffer_underruns_delta'] is None and stopped['jitter_buffer_depth'] is None and stopped['receive_bitrate'] is None,'stopped metrics null, not fake zero or underrun')
 total_a=stopped['cumulative']['received_packets'];pair_start('tx-b');wait(lambda:history('tx-b')['snapshots'][-1]['received_packets_delta'] is not None);time.sleep(1.2)
 check(history()['snapshots'][-1]['cumulative']['received_packets']==total_a,'switching output owner does not pollute A counters')
 check(history('tx-b')['snapshots'][-1]['cumulative']['received_packets']>0,'B has independently attributed counters')
 for id in ['tx-a','tx-b']:
  for range in ['60s','5m','15m']:
   status,h=request(f'/telemetry/history?device_id={id}&range={range}');check(status==200 and h['range']==range and all(s['device_id']==id for s in h['snapshots']),f'History {id} {range}')
   check(h['snapshots']==sorted(h['snapshots'],key=lambda s:(s['timestamp'],s['sequence'])),'history timestamp order')
 check('STOP_STREAM' in {e['event_type'] for e in history()['events']},'STOP_STREAM event retained')
 fresh=history();check(len(fresh['snapshots'])>=10,'fresh client retrieves retained history')
 result={'checks':checks,'history_a':history(),'history_b':history('tx-b')};(ROOT/'artifacts/telemetry-integration-result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf8')
 print(f'{len(checks)} telemetry integration checks passed',flush=True)
finally:
 for process,f in reversed(processes):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();process.wait()
  f.close()
