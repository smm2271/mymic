import { Component, signal, OnDestroy } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TelemetryChart } from './telemetry-chart';
import { History,HistoryState,TelemetrySnapshot,TelemetryEvent,emptyHistory,restoreHistory,appendSnapshot,appendEvent,loss60 } from './history-model';
interface Device { id:string; name:string; device_type:string; address:string; state:string; capabilities:string[]; source:string; gain:number; mute:boolean; last_error:string|null }
interface Output {id:string;name:string;is_default:boolean}
@Component({selector:'app-root',imports:[FormsModule,TelemetryChart],templateUrl:'./app.html',styleUrl:'./app.css'})
export class App implements OnDestroy {
 readonly online=signal(false);readonly error=signal('');readonly devices=signal<Device[]>([]);readonly outputs=signal<Output[]>([]);readonly metrics=signal<Record<string,any>>({});readonly busy=signal('');readonly socketState=signal('連線中');
 readonly history=signal<HistoryState>(emptyHistory());readonly historyError=signal('');readonly loadingHistory=signal(false);readonly selectedDevice=signal('');readonly range=signal('60s');readonly chartTimezone=signal<'local'|'utc'>('local');private historyRequest=0;
 readonly charts=[{field:'packet_loss_rate' as const,title:'播放期限缺包率（非 UDP 遺失）',unit:'%',color:'#ecad75'},{field:'receive_bitrate' as const,title:'每秒 PCM Payload 接收速率',unit:'kbps',color:'#68c9b4'},{field:'jitter_buffer_depth' as const,title:'Jitter Buffer Depth',unit:'封包',color:'#90b1ff'},{field:'buffer_underruns_per_second' as const,title:'Buffer Underruns',unit:'次／秒',color:'#dc94c9'},{field:'rx_packet_rate' as const,title:'TX / RX 封包率對照',unit:'packets/s',color:'#68c9b4'},{field:'tx_scheduling_drift_ms' as const,title:'TX Scheduling Drift（Simulator-only）',unit:'ms',color:'#90b1ff'}];
 code:Record<string,string>={};output='default';private token='';private socket?:WebSocket;private timer?:ReturnType<typeof setTimeout>;private destroyed=false;
 private readonly base=location.port==='4200'?'http://127.0.0.1:8080':'';
 readonly labels:Record<string,string>={OFFLINE:'離線',DISCOVERED:'待配對',PAIRING:'配對中',CONNECTED:'已連線',STREAMING:'串流中'};
 readonly sources=[{id:'sine',name:'440 Hz 正弦波'},{id:'sweep',name:'頻率掃描'},{id:'pulse',name:'脈衝'},{id:'silence',name:'靜音訊號'}];
 constructor(){void this.connect();}
 private async request(path:string,method='GET',body?:unknown){const r=await fetch(this.base+'/api/v1'+path,{method,headers:{'Content-Type':'application/json','X-Mic-Token':this.token},...(body===undefined?{}:{body:JSON.stringify(body)})});const data=await r.json();if(!r.ok)throw new Error(data.error?.message||`HTTP ${r.status}`);return data;}
 private async connect(){if(this.destroyed)return;try{const status=await this.request('/status');this.token=status.api_token;this.devices.set(await this.request('/devices'));this.ensureSelection();this.outputs.set(await this.request('/audio/outputs'));this.online.set(true);this.error.set('');const origin=this.base||location.origin;this.socket=new WebSocket(origin.replace(/^http/,'ws')+'/api/v1/events');this.socket.onopen=()=>{this.socketState.set('即時同步');void this.loadHistory();};this.socket.onmessage=(e)=>this.onMessage(JSON.parse(e.data));this.socket.onclose=()=>{this.online.set(false);this.socketState.set('連線中斷，正在重試');this.retry();};this.socket.onerror=()=>this.socket?.close();}catch(e){this.online.set(false);this.error.set('Rust 服務無法連線：'+String(e));this.retry();}}
 private retry(){if(!this.destroyed){clearTimeout(this.timer);this.timer=setTimeout(()=>void this.connect(),2000);}}
 async action(id:string,path:string,body:unknown,method='POST'){this.busy.set(id);this.error.set('');try{await this.request(path,method,body);}catch(e){this.error.set(String(e));}finally{this.busy.set('');}}
 pair(d:Device){void this.action(d.id,`/devices/${d.id}/pair`,{code:(this.code[d.id]||'').trim()});this.code[d.id]='';}
 reject(d:Device){void this.action(d.id,`/devices/${d.id}/pair`,{code:'',reject:true});}
 disconnect(d:Device){void this.action(d.id,`/devices/${d.id}/disconnect`,{});}
 audio(d:Device,body:unknown){void this.action(d.id,`/devices/${d.id}/audio`,body,'PATCH');}
 selectOutput(){void this.action('output','/audio/output',{id:this.output},'PUT');}
 private ensureSelection(){if(!this.devices().some(d=>d.id===this.selectedDevice())){const id=this.devices().find(d=>d.state==='STREAMING')?.id||this.devices()[0]?.id||'';if(id!==this.selectedDevice())this.selectDevice(id);}}
 selectDevice(id:string){this.selectedDevice.set(id);this.history.set(emptyHistory(id));void this.loadHistory();}
 selectRange(range:string){this.range.set(range);void this.loadHistory();}
 seconds(){return this.range()==='60s'?60:this.range()==='5m'?300:900;}
 private onMessage(message:any){
  if(message.kind==='telemetry_snapshot'){
   const snapshot=message.snapshot as TelemetrySnapshot;const old=this.history();
   this.history.set(appendSnapshot(old,message.stream_id,snapshot));
   if(snapshot.device_id===this.selectedDevice()&&old.streamId&&old.streamId!==message.stream_id)void this.loadHistory();
  }else if(message.kind==='telemetry_event'){this.history.set(appendEvent(this.history(),message.stream_id,message.event as TelemetryEvent));}
  else if(message.kind==='resync_required'){void this.loadHistory();}
  else if(Array.isArray(message.devices)){this.devices.set(message.devices);this.metrics.set(message.metrics);this.ensureSelection();}
 }
 async loadHistory(){
  const id=this.selectedDevice();if(!id)return;
  const requestId=++this.historyRequest;this.loadingHistory.set(true);this.historyError.set('');
  try{
   // Fetch all 15 minutes so the 60-second aggregate remains valid for every chart range.
   const response=await this.request(`/telemetry/history?device_id=${encodeURIComponent(id)}&range=15m`) as History;
   if(requestId!==this.historyRequest||id!==this.selectedDevice())return;
   this.history.set(restoreHistory(this.history(),response));
  }catch(e){if(requestId===this.historyRequest)this.historyError.set('歷史資料無法取得：'+String(e));}
  finally{if(requestId===this.historyRequest)this.loadingHistory.set(false);}
 }
 latest(){return this.history().snapshots.at(-1);}
 loss():string{const rate=loss60(this.history());return rate===null?'—':rate.toFixed(2);}
 rate():string{const rate=this.latest()?.receive_bitrate;return rate==null?'—':(rate/1000).toFixed(1);}
 totalLoss():string{const counters=this.latest()?.cumulative;if(!counters||counters.received_packets+counters.lost_packets===0)return '—';return (100*counters.lost_packets/(counters.received_packets+counters.lost_packets)).toFixed(2);}
 utc(timestamp:number|undefined){return timestamp===undefined?'—':new Date(timestamp).toISOString();}
 ngOnDestroy(){this.destroyed=true;clearTimeout(this.timer);this.socket?.close();}
}
