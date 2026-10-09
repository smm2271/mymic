"""Real Release background pipeline observer. Focus/audibility require manual verification."""
import argparse,ctypes,json,os,platform,subprocess,time,urllib.request
from pathlib import Path
parser=argparse.ArgumentParser();parser.add_argument('--seconds',type=int,default=600);parser.add_argument('--output',default='artifacts/background-audio-600s.json');args=parser.parse_args()
ROOT=Path(__file__).resolve().parents[1];BASE='http://127.0.0.1:18085/api/v1';CODE='0123456789abcdef0123456789abcdef';processes=[];TOKEN=''
class Memory(ctypes.Structure):
 _fields_=[('cb',ctypes.c_ulong),('faults',ctypes.c_ulong),('peak',ctypes.c_size_t),('working_set',ctypes.c_size_t),('p1',ctypes.c_size_t),('p2',ctypes.c_size_t),('p3',ctypes.c_size_t),('p4',ctypes.c_size_t),('pagefile',ctypes.c_size_t),('peak_pagefile',ctypes.c_size_t),('private',ctypes.c_size_t)]
def usage(process):
 c,e,k,u=[ctypes.c_uint64() for _ in range(4)];kernel=ctypes.WinDLL('kernel32');kernel.GetProcessTimes(ctypes.c_void_p(int(process._handle)),ctypes.byref(c),ctypes.byref(e),ctypes.byref(k),ctypes.byref(u));m=Memory();m.cb=ctypes.sizeof(m);ctypes.WinDLL('psapi').GetProcessMemoryInfo(ctypes.c_void_p(int(process._handle)),ctypes.byref(m),m.cb);return {'cpu_seconds':(k.value+u.value)/10000000,'working_set_bytes':m.working_set,'private_bytes':m.private}
def request(path,method='GET',body=None):
 req=urllib.request.Request(BASE+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json','X-Mic-Token':TOKEN})
 with urllib.request.urlopen(req,timeout=5) as r:return json.load(r)
def wait(fn):
 until=time.monotonic()+12
 while time.monotonic()<until:
  try:
   result=fn()
   if result:return result
  except (OSError,StopIteration,KeyError):pass
  time.sleep(.1)
 raise RuntimeError('wait timed out')
def launch(name,*args,env=None):
 f=open(ROOT/'artifacts'/f'background-{name}.log','w',encoding='utf8');child=subprocess.Popen([str(ROOT/'target/release'/f'{name}.exe'),*args],cwd=ROOT,stdout=f,stderr=f,env=env,creationflags=subprocess.CREATE_NO_WINDOW);processes.append((child,f));return child
try:
 (ROOT/'artifacts').mkdir(exist_ok=True)
 env=os.environ.copy();env.pop('MIC_DISABLE_AUDIO',None);env.update(MIC_HTTP_PORT='18085',MIC_DISCOVERY_PORT='41500',MIC_CONTROL_PORT='41501',MIC_AUDIO_PORT='41502')
 server=launch('mic-server',env=env);wait(lambda:request('/status')['service']=='online');TOKEN=request('/status')['api_token']
 simulator=launch('mic-simulator','--id','background-tx','--code',CODE,'--discovery','127.0.0.1:41500','--timing-mode','waitable','--timing-diagnostics','--diagnostics-file',str(ROOT/'artifacts/background-timing.jsonl'))
 wait(lambda:any(d['id']=='background-tx' for d in request('/devices')))
 request('/devices/background-tx/pair','POST',{'code':CODE});request('/devices/background-tx/audio','PATCH',{'gain':.02,'mute':False,'source':'sine','streaming':True})
 started=time.perf_counter();before=request('/metrics');cpu_before=[usage(p) for p,_ in processes];observations=[]
 while time.perf_counter()-started<args.seconds:
  m=request('/metrics');observation={'elapsed_seconds':time.perf_counter()-started,'metrics':m,'server':usage(server),'simulator':usage(simulator)};observations.append(observation)
  (ROOT/'artifacts/background-audio-progress.json').write_text(json.dumps(observation,indent=2),encoding='utf8')
  if round(observation['elapsed_seconds'])%30<5:print(f"{observation['elapsed_seconds']:.1f}s RX={m['received_packets']} underruns={m['buffer_underruns']} raw_callback_starvations={m.get('callback_starvations')} depth={m['jitter_buffer_depth']}",flush=True)
  time.sleep(min(5,max(0,args.seconds-(time.perf_counter()-started))))
 elapsed=time.perf_counter()-started;after=request('/metrics');cpu_after=[usage(p) for p,_ in processes]
 request('/devices/background-tx/audio','PATCH',{'streaming':False});time.sleep(.2)
 reports=[json.loads(l) for l in (ROOT/'artifacts/background-timing.jsonl').read_text().splitlines()];last=reports[-1]
 windows={key:{'p50':sorted(v)[round((len(v)-1)*.5)],'p95':sorted(v)[round((len(v)-1)*.95)],'p99':sorted(v)[round((len(v)-1)*.99)],'max':max(v)} for key in ['send_interval_samples_ms','scheduling_delay_samples_ms'] if (v:=[v for r in reports for v in r.get(key,[])])}
 result={'scenario':'CREATE_NO_WINDOW_background_redirected','windows_version':platform.platform(),'duration_seconds':elapsed,'before':before,'after':after,'tx_final':last,'timing_statistics':windows,'rx_packets_per_second':(after['received_packets']-before['received_packets'])/elapsed,'received_payload_kbps':(after['received_packets']-before['received_packets'])*320*8/elapsed/1000,'input_samples_per_second':(after.get('input_samples_consumed',0)-before.get('input_samples_consumed',0))/elapsed,'output_frames_per_second':(after.get('output_frames_requested',0)-before.get('output_frames_requested',0))/elapsed,'source_delivery_samples_per_second':(after.get('delivered_samples',0)-before.get('delivered_samples',0))/elapsed,'underruns_delta':after['buffer_underruns']-before['buffer_underruns'],'callback_starvations_delta':after.get('callback_starvations',0)-before.get('callback_starvations',0),'cpu_percent_one_core':[(a['cpu_seconds']-b['cpu_seconds'])/elapsed*100 for a,b in zip(cpu_after,cpu_before)],'observations':observations,'manual_listening':'Pending Manual Verification','window_focus':'Not controlled; actual foreground/minimized tests pending'}
 (ROOT/args.output).write_text(json.dumps(result,indent=2),encoding='utf8');print(json.dumps({k:v for k,v in result.items() if k not in ['observations','tx_final','before','after']},indent=2),flush=True)
finally:
 for process,f in reversed(processes):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();process.wait()
  f.close()
