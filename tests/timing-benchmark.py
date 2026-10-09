import socket,json,time,hmac,hashlib,subprocess,struct,statistics,argparse,platform,ctypes
parser=argparse.ArgumentParser();parser.add_argument('--mode',default='waitable');parser.add_argument('--seconds',type=int,default=60);parser.add_argument('--creation',choices=['hidden','inherited'],default='hidden');parser.add_argument('--output',default='artifacts/timing-waitable.json');parser.add_argument('--loss-every',type=int,default=0);args=parser.parse_args()
from pathlib import Path
ROOT=Path.cwd();CODE='0123456789abcdef0123456789abcdef';ID='raw-benchmark';SESSION='a'*32
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.setsockopt(socket.SOL_SOCKET,socket.SO_RCVBUF,1048576);s.bind(('127.0.0.1',41400));s.settimeout(.2)
a=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);a.setsockopt(socket.SOL_SOCKET,socket.SO_RCVBUF,1048576);a.bind(('127.0.0.1',41402));a.settimeout(.1)
def sign(m,key):
 fields=[str(m.get('version',1)),m['kind'],str(m['request_id']),m['device_id'],m.get('nonce',''),m.get('session',''),str(m.get('audio_port',0)),str(m.get('control_port',0)),m.get('source',''),str(round(m['gain']*1000)) if 'gain'in m else '',str(int(m['mute'])) if 'mute'in m else '']
 m['proof']=hmac.new(key,'|'.join(fields).encode(),hashlib.sha256).hexdigest();return m
ROOT.joinpath('artifacts').mkdir(exist_ok=True)
f=open(str(ROOT/args.output).replace('.json','.log'),'w');p=subprocess.Popen([str(ROOT/'target/release/mic-simulator.exe'),'--id',ID,'--code',CODE,'--discovery','127.0.0.1:41400','--timing-mode',args.mode,'--loss-every',str(args.loss_every),'--timing-diagnostics','--diagnostics-file',str(ROOT/args.output).replace('.json','.jsonl')],stdout=f,stderr=f,cwd=ROOT,creationflags=subprocess.CREATE_NO_WINDOW if args.creation=='hidden' else 0)
try:
 m,peer=None,None
 while m is None:
  try:b,peer=s.recvfrom(4096);d=json.loads(b);m=d if d['kind']=='DISCOVER' else None
  except socket.timeout:pass
 key=hmac.new(CODE.encode(),(m['nonce']+'|'+SESSION).encode(),hashlib.sha256).digest()
 pair=sign({'version':1,'kind':'PAIR_REQUEST','request_id':10,'device_id':ID,'nonce':m['nonce'],'session':SESSION,'audio_port':41402,'control_port':41400},CODE.encode());s.sendto(json.dumps(pair).encode(),peer)
 while True:
  try:b,_=s.recvfrom(4096)
  except socket.timeout:continue
  if json.loads(b)['kind']=='PAIR_ACCEPT':break
 start=sign({'version':1,'kind':'START_STREAM','request_id':11,'device_id':ID,'session':SESSION},key);s.sendto(json.dumps(start).encode(),peer)
 began=time.perf_counter();last_h=began;received=authenticated=rejected=duplicates=out_of_order=0;unique=set();intervals=[];previous_time=None;last_seq=None;highest=-1;last_timestamp=0;request=12
 while time.perf_counter()-began<args.seconds:
  now=time.perf_counter()
  if now-last_h>=1:
   heartbeat=sign({'version':1,'kind':'HEARTBEAT','request_id':request,'device_id':ID,'session':SESSION},key);request+=1;s.sendto(json.dumps(heartbeat).encode(),peer);last_h=now
  try:b,_=a.recvfrom(4096)
  except socket.timeout:continue
  received+=1;now=time.perf_counter()
  if len(b)!=368 or not hmac.compare_digest(b[-16:],hmac.new(key,b[:-16],hashlib.sha256).digest()[:16]):rejected+=1;continue
  authenticated+=1;seq,ts=struct.unpack_from('<II',b,6);last_timestamp=ts
  if seq in unique:duplicates+=1
  elif highest>=0 and seq<highest:out_of_order+=1
  unique.add(seq);highest=max(highest,seq)
  if previous_time is not None:intervals.append((now-previous_time)*1000)
  previous_time=now
 elapsed=time.perf_counter()-began;intervals.sort()
 percentile=lambda p:intervals[min(len(intervals)-1,round((len(intervals)-1)*p))]
 stop_message=sign({'version':1,'kind':'STOP_STREAM','request_id':request+1,'device_id':ID,'session':SESSION},key);s.sendto(json.dumps(stop_message).encode(),peer)
 time.sleep(.15)
 diagnostics=Path(str(ROOT/args.output).replace('.json','.jsonl'));reports=[json.loads(line) for line in diagnostics.read_text().splitlines()]
 result={'timing_mode':args.mode,'scenario':'headless_background_redirected' if args.creation=='hidden' else 'inherited_console_visibility_unverified','windows_version':platform.platform(),'tx_last_report':reports[-1] if reports else None,'timing_totals':{key:{'p50':statistics.median(values),'p95':sorted(values)[round((len(values)-1)*.95)],'p99':sorted(values)[round((len(values)-1)*.99)],'max':max(values)} for key in ['send_interval_samples_ms','scheduling_delay_samples_ms'] if (values:=[v for r in reports for v in r.get(key,[])])},'diagnostic_reports':len(reports),'reference_commit':'v0.5.2-working-tree','audio_output_enabled':False,'duration_seconds':elapsed,'received_datagrams':received,'authenticated_packets':authenticated,'rejected_packets':rejected,'unique_packets':len(unique),'sequence_gaps_observed':highest+1-len(unique),'duplicate_packets':duplicates,'out_of_order_packets':out_of_order,'rx_packet_rate':authenticated/elapsed,'payload_bitrate':authenticated*320*8/elapsed,'last_sequence':highest,'last_sample_timestamp':last_timestamp,'receive_interval_ms':{'p50':percentile(.5),'p95':percentile(.95),'p99':percentile(.99)},'tx_instrumentation':'dedicated TX thread counters and monotonic send timestamps'}
 (ROOT/args.output).write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2),flush=True)
finally:p.terminate();p.wait();f.close();s.close();a.close()
