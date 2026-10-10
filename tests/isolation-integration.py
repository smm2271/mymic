"""Startup switch/API provenance tests on isolated ports; no ESP32 claim."""
import json, os, subprocess, time, urllib.request
from pathlib import Path
root=Path(__file__).resolve().parents[1]
exe=root/'target'/os.environ.get('MIC_TEST_PROFILE','debug')/'mic-server.exe'
flags=['MIC_TEST_OUTPUT_FADE','MIC_TEST_CAPACITY_POLICY','MIC_TEST_RECOVERY_POLICY']
for changed in [None,*range(3)]:
    env=os.environ.copy()
    env.update(dict(zip(flags,['on']*3)))
    if changed is not None: env[flags[changed]]='off'
    env.update(MIC_DISABLE_AUDIO='1',MIC_HTTP_PORT='18085',MIC_DISCOVERY_PORT='41600',MIC_CONTROL_PORT='41601',MIC_AUDIO_PORT='41602')
    p=subprocess.Popen([str(exe)],cwd=root,env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    try:
        for attempt in range(100):
            try:
                with urllib.request.urlopen('http://127.0.0.1:18085/api/v1/status',timeout=1) as r: status=json.load(r)
                break
            except OSError:
                assert p.poll() is None, p.stderr.read().decode(errors='replace')
                time.sleep(.05)
        else: raise AssertionError('startup timeout')
        expected=[i!=changed for i in range(3)]
        assert [status['isolation'][n] for n in ['output_fade','capacity_policy','recovery_policy']]==expected
        assert len(status['build_commit'])==40
        assert status['build_version']=='v0.6.3-isolation-1'
        print('PASS isolated startup/API options',changed,flush=True)
    finally:
        p.terminate(); p.wait(timeout=5); p.stderr.close()
env['MIC_TEST_OUTPUT_FADE']='invalid'
p=subprocess.run([str(exe)],cwd=root,env=env,capture_output=True,timeout=5)
assert p.returncode!=0 and b'MIC_TEST_OUTPUT_FADE' in p.stderr
print('PASS invalid option fails startup')
