import assert from 'node:assert/strict';
const base=process.argv[2]||'http://127.0.0.1:8080';
const ws=new WebSocket(base.replace(/^http/,'ws')+'/api/v1/events');
const timeout=setTimeout(()=>{console.error('WebSocket timeout');process.exit(1)},5000);
ws.addEventListener('error',()=>{console.error('WebSocket connection failed');process.exit(1)});
ws.addEventListener('message',({data})=>{
 const snapshot=JSON.parse(data);assert.ok(Array.isArray(snapshot.devices));assert.equal(typeof snapshot.metrics.received_packets,'number');
 clearTimeout(timeout);ws.close();console.log('PASS WebSocket pushes device and audio snapshots');
},{once:true});
