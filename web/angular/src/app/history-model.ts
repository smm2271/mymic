export interface Counters {received_packets:number;lost_packets:number;out_of_order_packets:number;received_bytes:number;buffer_underruns:number;buffer_overruns:number}
export interface TelemetrySnapshot {id:string;sequence:number;timestamp:number;device_id:string;session_id:string|null;connection_state:string;streaming_state:boolean;interval_ms:number|null;received_packets_delta:number|null;lost_packets_delta:number|null;out_of_order_packets_delta:number|null;receive_bitrate:number|null;packet_loss_rate:number|null;jitter_buffer_depth:number|null;buffer_underruns_delta:number|null;buffer_overruns_delta:number|null;buffer_underruns_per_second:number|null;cumulative:Counters;discontinuity:string|null;rx_packet_rate?:number|null;tx_packet_rate?:number|null;tx_scheduling_drift_ms?:number|null;loss_basis?:string;bitrate_basis?:string;tx?:{timing_mode:string;generated_packets:number;successfully_sent_packets:number;send_errors:number;sequence_gaps_generated_by_tx:number;missed_deadlines:number}|null}
export interface TelemetryEvent {id:string;sequence:number;timestamp:number;device_id:string;session_id:string|null;event_type:string}
export interface History {stream_id:string;device_id:string;range:string;generated_at:number;snapshots:TelemetrySnapshot[];events:TelemetryEvent[];packet_loss_rate_60s:number|null;dropped_event_inputs:number}
export interface HistoryState {streamId:string;deviceId:string;snapshots:TelemetrySnapshot[];events:TelemetryEvent[];now:number}
export function emptyHistory(deviceId=''):HistoryState{return {streamId:'',deviceId,snapshots:[],events:[],now:0};}
function bounded<T extends {id:string;sequence:number;timestamp:number;device_id:string}>(items:T[],deviceId:string,now:number,capacity:number):T[]{
 const unique=new Map<string,T>();for(const item of items){if(item.device_id===deviceId&&item.timestamp>=now-900000&&item.timestamp<=now)unique.set(item.id,item);}
 return [...unique.values()].sort((a,b)=>a.timestamp-b.timestamp||a.sequence-b.sequence).slice(-capacity);
}
export function restoreHistory(current:HistoryState,response:History):HistoryState{
 const same=current.deviceId===response.device_id&&current.streamId===response.stream_id;
 const now=Math.max(response.generated_at,same?current.now:0);
 return {streamId:response.stream_id,deviceId:response.device_id,now,snapshots:bounded([...response.snapshots,...(same?current.snapshots:[])],response.device_id,now,900),events:bounded([...response.events,...(same?current.events:[])],response.device_id,now,4096)};
}
export function appendSnapshot(current:HistoryState,streamId:string,snapshot:TelemetrySnapshot):HistoryState{
 if(snapshot.device_id!==current.deviceId)return current;
 const state=current.streamId&&current.streamId!==streamId?emptyHistory(current.deviceId):current;
 const now=Math.max(state.now,snapshot.timestamp);
 return {...state,streamId,now,snapshots:bounded([...state.snapshots,snapshot],state.deviceId,now,900),events:bounded(state.events,state.deviceId,now,4096)};
}
export function appendEvent(current:HistoryState,streamId:string,event:TelemetryEvent):HistoryState{
 if(event.device_id!==current.deviceId)return current;
 const state=current.streamId&&current.streamId!==streamId?emptyHistory(current.deviceId):current;
 const now=Math.max(state.now,event.timestamp);
 return {...state,streamId,now,events:bounded([...state.events,event],state.deviceId,now,4096),snapshots:bounded(state.snapshots,state.deviceId,now,900)};
}
export function loss60(state:HistoryState):number|null{
 let received=0,lost=0,valid=false;
 for(const s of state.snapshots){if(s.timestamp>state.now-60000&&s.received_packets_delta!==null&&s.lost_packets_delta!==null){received+=s.received_packets_delta;lost+=s.lost_packets_delta;valid=true;}}
 return valid&&received+lost>0?100*lost/(received+lost):null;
}
