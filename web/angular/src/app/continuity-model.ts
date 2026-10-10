export interface ContinuityRecord {
  event_type:string;rx_monotonic_us:number;stream_id:number;device_id:string|null;
  session_id:string|null;packet_sequence:number|null;expected_sequence:number|null;
  jitter_buffer_depth:number;target_buffer_depth:number;ring_depth_samples:number|null;reason:string;callback_interval_us:number|null;callback_budget_us:number|null;
}
export interface DiagnosticsSnapshot {
  continuity:{enabled:boolean;drop_count:number;counts:number[];counts_60s:number[];
    event_types:string[];recent:ContinuityRecord[];event_log_path:string|null;event_log_error:string|null};
  capture:{active:boolean;samples:number;dropped_frames:number;reason:string|null;error:string|null;
    path:string|null;started_monotonic_us:number|null;ended_monotonic_us:number|null};
}
export function emptyDiagnostics():DiagnosticsSnapshot {
  return {continuity:{enabled:true,drop_count:0,counts:[],counts_60s:[],event_types:[],recent:[],event_log_path:null,event_log_error:null},
    capture:{active:false,samples:0,dropped_frames:0,reason:null,error:null,path:null,started_monotonic_us:null,ended_monotonic_us:null}};
}
