"""Short real Windows output-path capture check; no acoustic/ESP32 claims."""
import json, os, subprocess, time, urllib.request, wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = 'http://127.0.0.1:18083/api/v1'
TOKEN = ''
processes = []

def request(path, method='GET', body=None):
    req = urllib.request.Request(BASE + path, method=method,
        headers={'Content-Type':'application/json', 'X-Mic-Token':TOKEN},
        data=None if body is None else json.dumps(body).encode())
    with urllib.request.urlopen(req, timeout=6) as r:
        return json.load(r)

def wait(fn):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        try:
            value = fn()
            if value: return value
        except (OSError, KeyError, IndexError): pass
        time.sleep(.05)
    raise AssertionError('output-path condition timed out')

def launch(name, *args, env=None):
    log = open(ROOT/'artifacts'/f'diagnostics-output-{name}.log', 'w', encoding='utf8')
    p = subprocess.Popen([str(ROOT/'target'/os.environ.get('MIC_TEST_PROFILE','release')/f'{name}.exe'), *args], cwd=ROOT,
                         stdout=log, stderr=log, env=env)
    processes.append((p,log))

try:
    (ROOT/'artifacts').mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(MIC_HTTP_PORT='18083', MIC_DISCOVERY_PORT='41400', MIC_CONTROL_PORT='41401',
               MIC_AUDIO_PORT='41402', MIC_DISABLE_AUDIO='0',
               MIC_DIAGNOSTICS_DIR=str(ROOT/'artifacts/diagnostics-output'))
    launch('mic-server', env=env)
    wait(lambda: request('/status'))
    TOKEN = request('/status')['api_token']
    request('/audio/output', 'PUT', {'id':'default'})
    code = '0123456789abcdef0123456789abcdef'
    launch('mic-simulator', '--id', 'diag-output', '--discovery', '127.0.0.1:41400', '--code', code)
    wait(lambda: request('/devices'))
    request('/devices/diag-output/pair','POST',{'code':code})
    request('/devices/diag-output/audio','PATCH',{'streaming':True,'source':'sine','gain':.05})
    wait(lambda: request('/metrics')['input_samples_consumed'] > 320)
    before = request('/metrics')['input_samples_consumed']
    request('/diagnostics/capture/start','POST',{})
    wait(lambda: request('/diagnostics')['capture']['samples'] >= 8000)
    capture = request('/diagnostics/capture/stop','POST',{})
    assert capture['error'] is None and capture['dropped_frames'] == 0
    with wave.open(capture['path']) as w:
        assert (w.getframerate(),w.getnchannels(),w.getsampwidth(),w.getnframes()) == (16000,1,2,capture['samples'])
        assert any(w.readframes(w.getnframes())), 'configured sine must yield nonzero captured PCM'
    metadata = json.loads(Path(capture['metadata_path']).read_text())
    assert len(metadata['frames']) * 160 == capture['samples']
    assert metadata['device_id'] == 'diag-output'
    assert all(frame['sample_offset'] == i*160 for i,frame in enumerate(metadata['frames']))
    time.sleep(.2)
    rows = [json.loads(line) for line in Path(metadata['event_log_path']).read_text().splitlines()]
    callbacks = [r for r in rows if r.get('record_type') == 'audio_callback'
                 and r['stream_id'] == metadata['stream_id'] and
                 metadata['started_monotonic_us'] <= r['rx_monotonic_us'] <= metadata['ended_monotonic_us']]
    assert callbacks and all(r['session_id'] == metadata['session_id'] for r in callbacks)
    assert all(0 <= r['supplied_frames'] <= r['requested_frames'] for r in callbacks)
    assert any(r['consumed_input_samples'] > 0 for r in callbacks)
    assert metadata['observation_drop_count_end'] == metadata['observation_drop_count_start'] == 0
    metrics = request('/metrics')
    assert metrics['input_samples_consumed'] > before and metrics['output_sample_rate'] > 0
    (ROOT/'artifacts/diagnostics-output-result.json').write_text(json.dumps({'capture':capture,'output_sample_rate':metrics['output_sample_rate'],'output_channels':metrics['output_channels']}, indent=2),encoding='utf8')
    request('/devices/diag-output/audio','PATCH',{'streaming':False})
    print('PASS real CPAL output consumes PCM while nonempty S16LE/16k mono capture and frame sidecar remain valid')
finally:
    for p,log in reversed(processes):
        if p.poll() is None: p.terminate()
        try: p.wait(timeout=5)
        except subprocess.TimeoutExpired: p.kill();p.wait()
        log.close()
