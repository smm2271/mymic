"""WASAPI playback loopback only; no physical microphone, no manual listening claim."""
from pathlib import Path
import sys,json,wave
ROOT=Path(__file__).resolve().parents[1];sys.path.insert(0,str(ROOT/'artifacts/capture-python'))
import soundcard as sc
import numpy as np
speaker=sc.default_speaker();loopback=sc.get_microphone(id=speaker.id,include_loopback=True)
assert loopback.isloopback,'must use playback loopback, never physical input'
rate=48000;data=loopback.record(samplerate=rate,numframes=rate*10)
mono=data.mean(axis=1);spectrum=np.abs(np.fft.rfft(mono*np.hanning(len(mono))));freq=np.fft.rfftfreq(len(mono),1/rate);peak=float(freq[np.argmax(spectrum)])
blocks=mono[:len(mono)//480*480].reshape(-1,480);rms=np.sqrt(np.mean(blocks**2,axis=1));median=float(np.median(rms));quiet=rms<median*.1
longest=run=0
for q in quiet:run=run+1 if q else 0;longest=max(run,longest)
result={'capture':'WASAPI output loopback','output_device':speaker.name,'sample_rate':rate,'channels':int(data.shape[1]),'duration_seconds':len(data)/rate,'dominant_frequency_hz':peak,'median_10ms_rms':median,'quiet_10ms_blocks':int(quiet.sum()),'total_10ms_blocks':len(quiet),'longest_quiet_ms':longest*10,'manual_listening':'Pending Manual Verification','focus':'not controlled; background hidden simulator'}
with wave.open(str(ROOT/'artifacts/440hz-loopback.wav'),'wb') as f:
 f.setnchannels(data.shape[1]);f.setsampwidth(2);f.setframerate(rate);f.writeframes((np.clip(data,-1,1)*32767).astype('<i2').tobytes())
(ROOT/'artifacts/loopback-analysis.json').write_text(json.dumps(result,indent=2),encoding='utf8');print(json.dumps(result,indent=2),flush=True)
