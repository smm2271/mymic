import assert from 'node:assert/strict';
const base=process.argv[2];
const ws=new WebSocket(base.replace(/^http/,'ws')+'/api/v1/events');
const timeout=setTimeout(()=>{console.error('diagnostics WebSocket timeout');process.exit(1)},5000);
ws.addEventListener('error',()=>process.exit(1));
ws.addEventListener('message',({data})=>{
  const snapshot=JSON.parse(data);if(!Array.isArray(snapshot.devices))return;
  const {continuity,capture}=snapshot.diagnostics;
  assert.equal(typeof continuity.clock_id,'string');
  assert.equal(continuity.counts.length,continuity.event_types.length);
  assert.equal(continuity.counts_60s.length,continuity.event_types.length);
  assert.ok(continuity.recent.length<=128);
  assert.ok(continuity.event_types.includes('AUDIO_CALLBACK_STARVATION'));
  assert.equal(typeof capture.active,'boolean');
  assert.equal(typeof snapshot.metrics.inter_arrival_jitter_ms,'number');
  clearTimeout(timeout);ws.close();console.log('PASS diagnostics WebSocket payload preserves existing metrics');
});
