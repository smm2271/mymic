"""Observe an already approved stream; does not bypass pairing or start audio."""
import argparse,json,time,urllib.request,datetime
from pathlib import Path
parser=argparse.ArgumentParser();parser.add_argument('--hours',type=float,default=4);parser.add_argument('--url',default='http://127.0.0.1:8080');args=parser.parse_args()
root=Path(__file__).resolve().parents[1];(root/'artifacts').mkdir(exist_ok=True)
start=time.monotonic();failures=0;previous=None
with (root/'artifacts/soak.jsonl').open('w',encoding='utf8') as log:
 while time.monotonic()-start<args.hours*3600:
  record={'time':datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).isoformat(),'elapsed_s':round(time.monotonic()-start,2)}
  try:
   with urllib.request.urlopen(args.url+'/api/v1/devices',timeout=5) as r:devices=json.load(r)
   with urllib.request.urlopen(args.url+'/api/v1/metrics',timeout=5) as r:metrics=json.load(r)
   assert any(d['state']=='STREAMING' for d in devices),'no streaming device'
   if previous is not None:assert metrics['received_packets']>previous,'packet reception stalled'
   previous=metrics['received_packets'];record.update(devices=devices,metrics=metrics)
  except Exception as e:failures+=1;record['error']=str(e)
  log.write(json.dumps(record,ensure_ascii=False)+'\n');log.flush();print(json.dumps(record,ensure_ascii=False),flush=True);time.sleep(5)
print(f'Observed {time.monotonic()-start:.1f}s; failing observations={failures}')
raise SystemExit(1 if failures else 0)
