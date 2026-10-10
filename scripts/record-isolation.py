"""Read-only one-Hz A/B snapshots; no playback/capture settings are changed."""
import argparse, datetime, json, time, urllib.request
from pathlib import Path
p=argparse.ArgumentParser()
p.add_argument('--port',type=int,default=8080)
p.add_argument('--seconds',type=int,default=30)
p.add_argument('--label',choices=['all-on','fade-off','capacity-old','recovery-old','all-old','baseline'],default='all-on')
a=p.parse_args()
if not 1<=a.seconds<=600: p.error('--seconds must be 1..600')
base=f'http://127.0.0.1:{a.port}/api/v1'
def get(path):
    with urllib.request.urlopen(base+path,timeout=3) as r: return json.load(r)
status=get('/status')
directory=Path('Diagnostics'); directory.mkdir(exist_ok=True)
file=directory/f'ab-{a.label}-{datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")}.jsonl'
with file.open('x',encoding='utf8') as out:
    out.write(json.dumps({'metadata':status,'label':a.label,'collector_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()})+'\n')
    origin=time.monotonic()
    for n in range(a.seconds):
        remaining=origin+n-time.monotonic()
        if remaining>0: time.sleep(remaining)
        data={'collector_elapsed_s':time.monotonic()-origin,'metrics':get('/metrics'),'diagnostics':get('/diagnostics')}
        out.write(json.dumps(data)+'\n'); out.flush()
print(file)
