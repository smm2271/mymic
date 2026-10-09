import { AfterViewInit, Component, ElementRef, Input, OnChanges, OnDestroy, ViewChild, signal } from '@angular/core';
import { Chart, ChartConfiguration, ChartDataset, registerables } from 'chart.js';
import annotationPlugin from 'chartjs-plugin-annotation';
import { TelemetrySnapshot, TelemetryEvent } from './history-model';
Chart.register(...registerables,annotationPlugin);
type Point={x:number;y:number|null};
@Component({selector:'app-telemetry-chart',template:`<section class="chart-card"><h3>{{title}} <small>{{unit}}</small></h3><div class="plot"><canvas #canvas role="img" [attr.aria-label]="title+'歷史折線圖，UTC 時間'"></canvas>@if(!hasData()){<div class="no-data">尚無適用資料<br><small>等待串流與下一個完整採樣區間</small></div>}</div><p class="event-hover" aria-live="polite">{{eventHover()||'移動滑鼠查看數值；事件線可查看串流／連線變更（UTC）'}}</p></section>`,styles:[`:host{display:block;min-width:0}.chart-card{border:1px solid #2d3d50;border-radius:12px;padding:18px;background:#101a27}h3{font-size:15px;margin:0 0 16px}small{color:#95a8bb;font-size:11px;font-weight:400}.plot{height:220px;position:relative}.no-data{position:absolute;inset:30% 10%;text-align:center;color:#a1aebc;pointer-events:none;line-height:1.8}.event-hover{font-size:11px;color:#a1aebc;min-height:32px;line-height:1.5;margin:12px 0 0;overflow-wrap:anywhere}`]})
export class TelemetryChart implements AfterViewInit,OnChanges,OnDestroy {
 @Input({required:true}) title='';@Input({required:true}) field: 'packet_loss_rate'|'receive_bitrate'|'jitter_buffer_depth'|'buffer_underruns_per_second'='packet_loss_rate';
 @Input() unit='';@Input() color='#68c9b4';@Input() snapshots:TelemetrySnapshot[]=[];@Input() events:TelemetryEvent[]=[];@Input() seconds=60;@Input() now=0;
 @ViewChild('canvas') canvas!:ElementRef<HTMLCanvasElement>;readonly hasData=signal(false);readonly eventHover=signal('');private chart?:Chart<'line',Point[]>;
 ngAfterViewInit(){this.render();}ngOnChanges(){if(this.canvas)this.render();}ngOnDestroy(){this.chart?.destroy();}
 private render(){
  const end=this.now||Date.now(),start=end-this.seconds*1000;
  const data:Point[]=this.snapshots.filter(s=>s.timestamp>=start&&s.timestamp<=end).map(s=>({x:s.timestamp,y:s.discontinuity?null:s[this.field]===null?null:s[this.field]!/(this.field==='receive_bitrate'?1000:1)}));
  this.hasData.set(data.some(p=>p.y!==null));
  const annotations:Record<string,any>={};
  for(const event of this.events.filter(e=>e.timestamp>=start&&e.timestamp<=end)){
   annotations[event.id]={type:'line',xMin:event.timestamp,xMax:event.timestamp,borderColor:'#efba74',borderWidth:1,borderDash:[3,3],hitTolerance:5,enter:()=>{this.eventHover.set(`${new Date(event.timestamp).toISOString()} · ${event.event_type} · ${event.device_id} · Session ${event.session_id||'—'}`);},leave:()=>this.eventHover.set('')};
  }
  const dataset:ChartDataset<'line',Point[]>={label:this.title,data,borderColor:this.color,backgroundColor:this.color,borderWidth:2,pointRadius:0,pointHoverRadius:4,pointHitRadius:8,spanGaps:false,tension:0};
  const config:ChartConfiguration<'line',Point[]>={type:'line',data:{datasets:[dataset]},options:{responsive:true,maintainAspectRatio:false,animation:false,parsing:false,interaction:{mode:'nearest',axis:'x',intersect:false},plugins:{legend:{display:false},annotation:{annotations},tooltip:{callbacks:{title:(items)=>items.length?new Date(items[0].parsed.x??0).toISOString():'',label:(item)=>`${this.title}：${item.parsed.y===null?'無資料':item.parsed.y.toFixed(3)} ${this.unit}`}}},scales:{x:{type:'linear',min:start,max:end,ticks:{maxTicksLimit:5,color:'#95a8bb',callback:(value)=>new Date(Number(value)).toISOString().slice(11,19)},grid:{color:'#243247'},title:{display:true,text:'UTC',color:'#95a8bb'}},y:{min:0,...(this.field==='packet_loss_rate'?{suggestedMax:1}:{}),ticks:{color:'#95a8bb',precision:this.field==='jitter_buffer_depth'?0:undefined},grid:{color:'#243247'}}}}};
  if(!this.chart){this.chart=new Chart(this.canvas.nativeElement,config);}else{this.chart.data=config.data;this.chart.options=config.options!;this.chart.update('none');}
 }
}
