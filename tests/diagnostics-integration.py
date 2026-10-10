"""Real local UDP/API/WebSocket checks. No ESP32, no audio hardware claim."""
import json, os, subprocess, time, urllib.request, urllib.error, wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = 'http://127.0.0.1:18082/api/v1'
CODE = '0123456789abcdef0123456789abcdef'
TOKEN = ''
processes = []

def request(path, method='GET', body=None, token=True):
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['X-Mic-Token'] = TOKEN
    req = urllib.request.Request(BASE + path, headers=headers, method=method,
                                 data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req, timeout=6) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, json.load(e)

def wait(fn, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = fn()
            if result:
                return result
        except (OSError, KeyError, IndexError):
            pass
        time.sleep(.1)
    raise AssertionError('condition timed out')

def launch(name, *args, env=None):
    log = open(ROOT / 'artifacts' / f'diagnostics-{name}.log', 'w', encoding='utf8')
    process = subprocess.Popen([str(ROOT / 'target' / os.environ.get('MIC_TEST_PROFILE', 'debug') / f'{name}.exe'), *args],
                               cwd=ROOT, stdout=log, stderr=log, env=env)
    processes.append((process, log))

try:
    (ROOT / 'artifacts').mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(MIC_HTTP_PORT='18082', MIC_DISCOVERY_PORT='41300', MIC_CONTROL_PORT='41301',
               MIC_AUDIO_PORT='41302', MIC_DISABLE_AUDIO='1',
               MIC_DIAGNOSTICS_DIR=str(ROOT / 'artifacts' / 'diagnostics-integration'))
    launch('mic-server', env=env)
    wait(lambda: request('/status')[0] == 200)
    TOKEN = request('/status')[1]['api_token']
    status = request('/diagnostics')[1]
    assert not status['capture']['active'] and not status['continuity']['recent']
    assert request('/diagnostics/capture/start', 'POST', {}, token=False)[0] == 403
    assert request('/diagnostics', 'PATCH', {'enabled': True, 'extra': 1})[0] == 400
    assert request('/diagnostics/capture/start', 'POST', {})[0] == 200
    assert request('/diagnostics/capture/start', 'POST', {})[0] in (400, 409)
    stopped = request('/diagnostics/capture/stop', 'POST', {})[1]
    assert stopped['reason'] == 'manual_stop' and stopped['samples'] == 0
    with wave.open(stopped['path']) as w:
        assert (w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()) == (16000, 1, 2, 0)
    print('PASS authenticated capture API, default off, manual stop, valid empty headless WAV', flush=True)
    launch('mic-simulator', '--id', 'diag-tx', '--discovery', '127.0.0.1:41300', '--code', CODE,
           '--loss-every', '13', '--reorder-every', '7')
    wait(lambda: len(request('/devices')[1]) == 1)
    assert request('/devices/diag-tx/pair', 'POST', {'code': CODE})[0] == 200
    assert request('/devices/diag-tx/audio', 'PATCH', {'streaming': True, 'mute': True})[0] == 200
    wait(lambda: request('/diagnostics')[1]['continuity']['counts'][0] > 0)
    continuity = request('/diagnostics')[1]['continuity']
    assert continuity['drop_count'] == 0 and continuity['recent']
    event = continuity['recent'][-1]
    assert event['device_id'] == 'diag-tx' and event['clock_id'] == continuity['clock_id']
    assert event['session_id'] and event['stream_id'] > 0
    assert event['rx_monotonic_us'] <= continuity['now_monotonic_us']
    assert {'expected_sequence', 'packet_sequence', 'jitter_buffer_depth', 'target_buffer_depth', 'reason'} <= event.keys()
    wait(lambda: request('/diagnostics')[1]['continuity']['event_log_path'])
    print('PASS real local UDP fault events, identifiers, timestamps and JSONL sink', flush=True)
    subprocess.run(['node', str(ROOT / 'tests/diagnostics-websocket.mjs'), 'http://127.0.0.1:18082'], check=True, timeout=8)
    assert request('/diagnostics', 'PATCH', {'enabled': False})[0] == 200
    before = request('/diagnostics')[1]['continuity']['counts']
    received = request('/metrics')[1]['received_packets']
    time.sleep(.7)
    assert request('/diagnostics')[1]['continuity']['counts'] == before
    assert request('/metrics')[1]['received_packets'] > received
    assert request('/diagnostics', 'PATCH', {'enabled': True})[0] == 200
    print('PASS diagnostic disable leaves authenticated audio reception running', flush=True)
    started = request('/diagnostics/capture/start', 'POST', {})[1]
    wait(lambda: not request('/diagnostics')[1]['capture']['active'], 33)
    ended = request('/diagnostics')[1]['capture']
    assert ended['reason'] == 'timeout_30s' and ended['samples'] <= 480000
    assert 30_000_000 <= ended['ended_monotonic_us'] - started['started_monotonic_us'] < 32_000_000
    metadata = json.loads(Path(ended['metadata_path']).read_text())
    assert metadata['clock_id'] == continuity['clock_id'] and metadata['frames'] == []
    print('PASS actual wall-clock 30-second auto-stop and sidecar clock correlation (headless)', flush=True)
finally:
    for p, log in reversed(processes):
        if p.poll() is None:
            p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait()
        log.close()
