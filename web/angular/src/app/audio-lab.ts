import {Component,input,signal} from '@angular/core';
import {FormsModule} from '@angular/forms';
import {baseline,counters,finish,Experiment,LabConfig,quantile,rates} from './audio-lab-model';
@Component({selector:'app-audio-lab',imports:[FormsModule],templateUrl:'./audio-lab.html',styleUrl:'./app.css'})
export class AudioLab {
 readonly devices=input<any[]>([]);readonly snapshot=input<any>({});readonly diagnostics=input<any>({});readonly online=input(false);readonly apply=input.required<(id:string,c:LabConfig)=>Promise<void>>();readonly version=input('unknown');
 id='';config=baseline();notes='';name='Baseline';readonly busy=signal(false);readonly error=signal('');readonly running=signal<Experiment|null>(null);readonly experiments=signal<Experiment[]>(this.restore());a=0;b=1;
 private restore():Experiment[]{try{return JSON.parse(localStorage.getItem('mymic-audio-lab-v1')||'[]').filter((e:any)=>e.version===1).slice(-20);}catch{return [];}}
 device(){return this.devices().find(d=>d.id===this.id);}
 state(){return this.snapshot().lab?.[this.id];}
 currentConfig():LabConfig {return this.state()?.applied||{...baseline(),jitter_buffer_target_ms:this.snapshot().active_device===this.id?(this.snapshot().metrics?.target_buffer_depth||3)*10:30};}
 selectDevice(id:string){this.id=id;this.config=structuredClone(this.currentConfig());}
 targetMs(){return this.state()?.applied?.jitter_buffer_target_ms??(this.snapshot().active_device===this.id?this.currentConfig().jitter_buffer_target_ms:null);}
 canStart(){const s=this.snapshot(),d=this.diagnostics();return this.online()&&s.active_device===this.id&&this.device()?.state==='STREAMING'&&s.diagnostic_stream_id===d.current_stream_id&&!['requested','failed'].includes(this.state()?.status);}
 supported(){return this.online()&&this.state()?.status!=='requested'&&this.device()?.capabilities.includes('audio_lab_v1')&&['CONNECTED','STREAMING'].includes(this.device()?.state);}
 async configure(reset=false){this.busy.set(true);this.error.set('');try {await this.apply()(this.id,reset?baseline():this.config);if(reset)this.config=baseline();}catch(e){this.error.set(String(e));}finally{this.busy.set(false);}}
 private timer=setInterval(()=>this.sample(),500);
 ngOnDestroy(){clearInterval(this.timer);}
 start(){const d=this.diagnostics(),s=this.snapshot();if(!this.canStart())return;this.running.set({version:1,name:this.name,notes:this.notes,device:this.id,session:d.current_session_id,stream:d.current_stream_id,clock:d.clock_id,config:structuredClone(this.currentConfig()),wireVersion:this.state()?.applied?2:1,physicalCapacityPackets:s.metrics?.jitter_capacity_packets,txVersion:this.device()?.capabilities.find((c:string)=>c.startsWith('tx_build:'))||'not reported',rxVersion:this.version(),startedUtc:new Date().toISOString(),startMono:s.counter_monotonic_us,diagnosticStartMono:d.now_monotonic_us,startCounters:counters(s.metrics,s.fec,d),invalid:d.enabled?[]:["diagnostic events not collected (disabled)"],diagnosticChanges:d.enabled_changes,diagnosticDrops:d.drop_count,samples:0,arrivalSamplesMs:[],callbackSamplesUs:[],measuredEndToEndMs:null,artifacts:{event_log:d.event_log_path,manifest:d.drift_manifest_path,summary:d.drift_summary_path},sampling:'500 ms WebSocket snapshot samples; not every packet/callback. Counter deltas use RX monotonic window.'});}
 private sample(){const e=this.running();if(!e)return;const s=this.snapshot(),d=this.diagnostics();if(!this.online()||d.current_stream_id!==e.stream||d.clock_id!==e.clock||s.active_device!==e.device||this.device()?.state!=='STREAMING'){if(!e.invalid.includes('stream/session/service interrupted'))e.invalid.push('stream/session/service interrupted');this.stop();return;}if(e.samples>=1800){e.invalid.push('15 minute recording bound reached');this.stop();return;}if(d.enabled_changes!==e.diagnosticChanges||d.drop_count!==e.diagnosticDrops){if(!e.invalid.includes('diagnostic event population incomplete'))e.invalid.push('diagnostic event population incomplete');}e.samples++;const m=s.metrics||{};if(m.inter_arrival_interval_ms!=null)e.arrivalSamplesMs.push(m.inter_arrival_interval_ms);const v=m.output_diagnostics?.callback_last_interval_us;if(v>0)e.callbackSamplesUs.push(v);this.running.set({...e});}
 stop(){const e=this.running();if(!e)return;const d=this.diagnostics(),s=this.snapshot();const result=finish(e,s.counter_monotonic_us,counters(s.metrics,s.fec,d),d.now_monotonic_us);const list=[...this.experiments(),result].slice(-20);this.experiments.set(list);this.running.set(null);try{localStorage.setItem('mymic-audio-lab-v1',JSON.stringify(list));}catch{this.error.set('Browser storage failed; export JSON now.');}}
 export(){const blob=new Blob([JSON.stringify(this.experiments(),null,2)],{type:'application/json'});const url=URL.createObjectURL(blob);const a=document.createElement('a');a.href=url;a.download='mymic-audio-lab-v1.json';a.click();URL.revokeObjectURL(url);}
 rates(e:Experiment){return JSON.stringify(rates(e));}
 q(e:Experiment){return JSON.stringify({p50:quantile(e.arrivalSamplesMs,.5),p95:quantile(e.arrivalSamplesMs,.95),p99:quantile(e.arrivalSamplesMs,.99),population:'500ms snapshot samples, ms'});}
 eventCount(kind:string){const d=this.diagnostics();if(!d.enabled)return null;const i=d.event_types?.indexOf(kind);return i>=0?d.counts[i]:null;}
 text(v:unknown){return JSON.stringify(v,null,2);}
}
