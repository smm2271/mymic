import { Component, signal, OnDestroy } from '@angular/core';
import { FormsModule } from '@angular/forms';
interface Device { id:string; name:string; device_type:string; address:string; state:string; capabilities:string[]; source:string; gain:number; mute:boolean; last_error:string|null }
interface Output {id:string;name:string;is_default:boolean}
@Component({selector:'app-root',imports:[FormsModule],templateUrl:'./app.html',styleUrl:'./app.css'})
export class App implements OnDestroy {
 readonly online=signal(false);readonly error=signal('');readonly devices=signal<Device[]>([]);readonly outputs=signal<Output[]>([]);readonly metrics=signal<Record<string,any>>({});readonly busy=signal('');readonly socketState=signal('連線中');
 code:Record<string,string>={};output='default';private token='';private socket?:WebSocket;private timer?:ReturnType<typeof setTimeout>;private destroyed=false;
 private readonly base=location.port==='4200'?'http://127.0.0.1:8080':'';
 readonly labels:Record<string,string>={OFFLINE:'離線',DISCOVERED:'待配對',PAIRING:'配對中',CONNECTED:'已連線',STREAMING:'串流中'};
 readonly sources=[{id:'sine',name:'440 Hz 正弦波'},{id:'sweep',name:'頻率掃描'},{id:'pulse',name:'脈衝'},{id:'silence',name:'靜音訊號'}];
 constructor(){void this.connect();}
 private async request(path:string,method='GET',body?:unknown){const r=await fetch(this.base+'/api/v1'+path,{method,headers:{'Content-Type':'application/json','X-Mic-Token':this.token},...(body===undefined?{}:{body:JSON.stringify(body)})});const data=await r.json();if(!r.ok)throw new Error(data.error?.message||`HTTP ${r.status}`);return data;}
 private async connect(){if(this.destroyed)return;try{const status=await this.request('/status');this.token=status.api_token;this.devices.set(await this.request('/devices'));this.outputs.set(await this.request('/audio/outputs'));this.online.set(true);this.error.set('');const origin=this.base||location.origin;this.socket=new WebSocket(origin.replace(/^http/,'ws')+'/api/v1/events');this.socket.onopen=()=>this.socketState.set('即時同步');this.socket.onmessage=(e)=>{const snapshot=JSON.parse(e.data);this.devices.set(snapshot.devices);this.metrics.set(snapshot.metrics);};this.socket.onclose=()=>{this.online.set(false);this.socketState.set('連線中斷，正在重試');this.retry();};this.socket.onerror=()=>this.socket?.close();}catch(e){this.online.set(false);this.error.set('Rust 服務無法連線：'+String(e));this.retry();}}
 private retry(){if(!this.destroyed){clearTimeout(this.timer);this.timer=setTimeout(()=>void this.connect(),2000);}}
 async action(id:string,path:string,body:unknown,method='POST'){this.busy.set(id);this.error.set('');try{await this.request(path,method,body);}catch(e){this.error.set(String(e));}finally{this.busy.set('');}}
 pair(d:Device){void this.action(d.id,`/devices/${d.id}/pair`,{code:(this.code[d.id]||'').trim()});this.code[d.id]='';}
 reject(d:Device){void this.action(d.id,`/devices/${d.id}/pair`,{code:'',reject:true});}
 disconnect(d:Device){void this.action(d.id,`/devices/${d.id}/disconnect`,{});}
 audio(d:Device,body:unknown){void this.action(d.id,`/devices/${d.id}/audio`,body,'PATCH');}
 selectOutput(){void this.action('output','/audio/output',{id:this.output},'PUT');}
 loss():string{const m=this.metrics();return ((m['lost_packets']||0)/Math.max(1,(m['received_packets']||0)+(m['lost_packets']||0))*100).toFixed(2);}
 ngOnDestroy(){this.destroyed=true;clearTimeout(this.timer);this.socket?.close();}
}
