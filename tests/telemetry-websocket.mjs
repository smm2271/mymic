import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {emptyHistory,restoreHistory,appendSnapshot,appendEvent} from '../web/angular/src/app/history-model.ts';
const base=process.argv[2],device=process.argv[3],out=process.argv[4];let state=emptyHistory(device),seen=new Set(),connections=0,live=0;
async function cycle(){
 connections++;let ws=new WebSocket(base.replace(/^http/,'ws')+'/api/v1/events');
 let done;const closed=new Promise(r=>done=r);let received=0;
 ws.addEventListener('message',({data})=>{const m=JSON.parse(data);if(m.kind==='telemetry_snapshot'&&m.snapshot.device_id===device){assert.ok(!seen.has(m.snapshot.id),'same snapshot pushed twice');seen.add(m.snapshot.id);live++;received++;state=appendSnapshot(state,m.stream_id,m.snapshot);}else if(m.kind==='telemetry_event')state=appendEvent(state,m.stream_id,m.event);});
 ws.addEventListener('error',()=>{console.error('unexpected WebSocket error');process.exit(1)});
 await new Promise(r=>ws.addEventListener('open',r,{once:true}));
 const response=await fetch(base+`/api/v1/telemetry/history?device_id=${device}&range=15m`).then(r=>r.json());state=restoreHistory(state,response);
 await new Promise(r=>setTimeout(r,2500));ws.close();ws.addEventListener('close',done,{once:true});await closed;assert.ok(received>=2,'telemetry delivered each second');
}
await cycle();await cycle();assert.equal(new Set(state.snapshots.map(s=>s.id)).size,state.snapshots.length);assert.ok(live>=4);await fs.writeFile(out,JSON.stringify({connections,live,history_count:state.snapshots.length,unique_ids:true}));console.log('PASS real WebSocket reconnect/history synchronization without duplicate IDs');
