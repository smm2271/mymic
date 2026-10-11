import assert from 'node:assert/strict';
import {baseline,counters,finish,quantile,rates} from '../web/angular/src/app/audio-lab-model.ts';
const c=baseline();assert.equal(c.frame_duration_ms,10);assert.equal(c.fec.enabled,false);
const initial=counters({received_packets:100},{recovered:20},{event_types:['BUFFER_RESYNC'],counts:[7]});
const e={startMono:1000000,startCounters:initial,invalid:[]};
const end=finish(e,3000000,counters({received_packets:130},{recovered:24},{event_types:['BUFFER_RESYNC'],counts:[9]}));
assert.equal(end.durationSeconds,2);assert.deepEqual(end.delta,{received_packets:30,fec_recovered:4,BUFFER_RESYNC:2});assert.deepEqual(end.invalid,[]);
assert.ok(finish(e,4000000,{received_packets:1}).invalid.length>0);
assert.equal(quantile([],0.5),null);assert.equal(quantile([20,1,10],.5),10);
console.log('PASS Audio Lab baseline, experiment deltas/window, reset/unavailable, sampled quantiles');

const load=rates({durationSeconds:2,delta:{authenticated_audio_packets:200,received_datagrams:250,received_datagram_bytes:53000}});assert.equal(load.authenticated_audio_pps,100);assert.equal(load.socket_datagram_pps,125);assert.equal(load.socket_udp_payload_kbps,212);
