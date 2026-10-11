use anyhow::{Result, anyhow};
use mic_audio::fec::{FecDecoder, FecMetrics};
use mic_audio::{Engine, Metrics, Playback};
use mic_device::{Device, State};
use mic_protocol::lab::{CAPABILITY, LabPacket, TransportConfig};
use mic_protocol::{AudioPacket, Message, Source, commitment, session_key};
#[derive(Clone, Default, Serialize)]
pub struct LabState {
    pub requested: Option<TransportConfig>,
    pub applied: Option<TransportConfig>,
    pub status: String,
    pub error: Option<String>,
    pub stream: u32,
}
use mic_telemetry::{Counters, DeviceInput, EventType, Telemetry};
use serde::Serialize;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{RwLock, mpsc, oneshot},
    time,
};
#[cfg(test)]
mod lab_control_tests;
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub metrics: Metrics,
    pub active_device: Option<String>,
    pub lab: HashMap<String, LabState>,
    pub fec: FecMetrics,
    pub counter_monotonic_us: u64,
    pub diagnostic_stream_id: u64,
}
#[derive(Clone)]
pub struct Core {
    pub snapshot: Arc<RwLock<Snapshot>>,
    pub playback: Playback,
    pub telemetry: Telemetry,
    tx: mpsc::Sender<Command>,
}
pub struct Command {
    pub id: String,
    pub action: Action,
    pub reply: oneshot::Sender<Result<(), String>>,
}
#[derive(Debug)]
pub enum Action {
    Pair(String),
    Transport(TransportConfig),
    Reject,
    Disconnect,
    Audio {
        source: Option<Source>,
        gain: Option<f32>,
        mute: Option<bool>,
        streaming: Option<bool>,
    },
}
struct Entry {
    lab: LabState,
    device: Device,
    peer: SocketAddr,
    nonce: String,
    commitment: String,
    code: Option<String>,
    key: [u8; 32],
    session: String,
    last: Instant,
    pending: Option<Pending>,
    desired: bool,
    last_request: u32,
    rejected_nonce: Option<String>,
    counters: Counters,
    activity_epoch: u64,
    accepted_session: Option<String>,
    tx: Option<(mic_protocol::diagnostics::TxTiming, Instant, u64)>,
}
struct Pending {
    messages: Vec<Message>,
    position: usize,
    reply: Option<oneshot::Sender<Result<(), String>>>,
    sent: Instant,
    retries: u8,
}
#[derive(Clone)]
pub struct Config {
    pub discovery: u16,
    pub control: u16,
    pub audio: u16,
    pub jitter: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            discovery: 41000,
            control: 41001,
            audio: 41002,
            jitter: 3,
        }
    }
}
fn random_hex() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}
fn emit_fec(engine: &Engine, fec: &mut FecDecoder) {
    while let Some((kind, sequence, reason)) = fec.take_notice() {
        engine.playback.diagnostics.emit_deadline(
            kind,
            Some(sequence),
            engine.jitter.expected_sequence(),
            engine.jitter.occupancy(),
            engine.jitter.depth,
            None,
            reason,
            fec.deadline_sequence(),
        );
    }
}
async fn send(socket: &UdpSocket, m: &Message, peer: SocketAddr) {
    if let Ok(b) = serde_json::to_vec(m) {
        let _ = socket.send_to(&b, peer).await;
    }
}
fn fail(e: &mut Entry, error: &str) {
    if let Some(mut p) = e.pending.take()
        && let Some(reply) = p.reply.take()
    {
        let _ = reply.send(Err(error.into()));
    }
    e.device.last_error = Some(error.into());
    if e.lab.status == "requested" {
        e.lab.status = "failed".into();
        e.lab.error = Some(error.into());
    }
}
fn pair_message(
    e: &mut Entry,
    code: String,
    request: u32,
    audio_port: u16,
    control_port: u16,
) -> Message {
    e.session = random_hex();
    e.key = session_key(&code, &e.nonce, &e.session);
    e.code = Some(code.clone());
    e.device.state = State::Pairing;
    let mut m = Message::new("PAIR_REQUEST", &e.device.id, request);
    m.nonce = e.nonce.clone();
    m.session = e.session.clone();
    m.audio_port = audio_port;
    m.control_port = control_port;
    m.sign(code.as_bytes());
    m
}
fn control(e: &Entry, kind: &str, request: u32) -> Message {
    let mut m = Message::new(kind, &e.device.id, request);
    m.session = e.session.clone();
    m.sign(&e.key);
    m
}
impl Core {
    pub async fn start(
        config: Config,
    ) -> Result<(Self, tokio::task::JoinHandle<()>, oneshot::Sender<()>)> {
        let discovery = UdpSocket::bind(("0.0.0.0", config.discovery)).await?;
        let control_socket = UdpSocket::bind(("0.0.0.0", config.control)).await?;
        let audio = UdpSocket::bind(("0.0.0.0", config.audio)).await?;
        let engine = Engine::with_options(config.jitter, mic_audio::IsolationOptions::from_env()?);
        let playback = engine.playback.clone();
        let snapshot = Arc::new(RwLock::new(Snapshot {
            devices: vec![],
            metrics: engine.metrics(),
            active_device: None,
            lab: HashMap::new(),
            fec: FecMetrics::default(),
            counter_monotonic_us: 0,
            diagnostic_stream_id: 0,
        }));
        let (tx, rx) = mpsc::channel(32);
        let (stop, stopped) = oneshot::channel();
        let (telemetry, telemetry_task) = Telemetry::start();
        let core = Self {
            snapshot: snapshot.clone(),
            playback,
            telemetry: telemetry.clone(),
            tx,
        };
        let task = tokio::spawn(async move {
            run(
                config,
                (discovery, control_socket, audio),
                engine,
                snapshot,
                rx,
                stopped,
                telemetry,
            )
            .await;
            telemetry_task.abort();
            let _ = telemetry_task.await;
        });
        Ok((core, task, stop))
    }
    pub async fn command(&self, id: String, action: Action) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command { id, action, reply })
            .await
            .map_err(|_| "service stopped".to_string())?;
        time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "command timed out".to_string())?
            .map_err(|_| "service stopped".to_string())?
    }
}
async fn run(
    config: Config,
    sockets: (UdpSocket, UdpSocket, UdpSocket),
    mut engine: Engine,
    snapshot: Arc<RwLock<Snapshot>>,
    mut commands: mpsc::Receiver<Command>,
    mut stopped: oneshot::Receiver<()>,
    telemetry: Telemetry,
) {
    let (discovery, socket, audio) = sockets;
    let mut entries: HashMap<String, Entry> = HashMap::new();
    let mut active: Option<String> = None;
    let mut request = rand::random::<u32>();
    let mut db = [0u8; 2049];
    let mut cb = [0u8; 2049];
    let mut ab = [0u8; 4096];
    let mut tick = time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Burst);
    let mut maintenance = time::interval(Duration::from_millis(250));
    let mut heartbeat = Instant::now();
    let mut audio_baseline = audio_counters(&engine);
    let counter_clock = Instant::now();
    let mut fec = FecDecoder::default();
    loop {
        tokio::select! {
                                                                    _=&mut stopped=>break,
                                                                    Ok((n,peer))=discovery.recv_from(&mut db)=>{
                                                                     let Ok(m)=Message::decode(&db[..n]) else{continue};if m.kind!="DISCOVER" || m.nonce.len()!=32 || m.commitment.len()!=64 || m.control_port==0 || hex::decode(&m.nonce).is_err() || hex::decode(&m.commitment).is_err(){continue}
                                                                     if !entries.contains_key(&m.device_id) && entries.len()>=64 {continue}
                                                                     let target=SocketAddr::new(peer.ip(),m.control_port);
                                                                     let e=entries.entry(m.device_id.clone()).or_insert_with(||Entry{lab:LabState::default(),device:Device{id:m.device_id.clone(),name:m.name.clone(),device_type:m.device_type.clone(),capabilities:m.capabilities.clone(),address:target.to_string(),state:State::Discovered,source:Source::Sine,gain:1.0,mute:false,last_error:None},peer:target,nonce:m.nonce.clone(),commitment:m.commitment.clone(),code:None,key:[0;32],session:String::new(),last:Instant::now(),pending:None,desired:false,last_request:0,rejected_nonce:None,counters:Counters::default(),activity_epoch:0,accepted_session:None,tx:None});
                                                                     if e.rejected_nonce.as_ref()==Some(&m.nonce) {continue}
                                               if matches!(e.device.state,State::Connected|State::Streaming|State::Pairing){continue}
                                                                     // Reconnecting a known identity requires the previous credential to verify the fresh commitment.
                                                                     if let Some(code)=&e.code&& commitment(code,&m.nonce)!=m.commitment {continue}
                                                                     e.rejected_nonce=None;e.device.capabilities=m.capabilities.clone();e.peer=target;e.device.address=target.to_string();e.nonce=m.nonce.clone();e.commitment=m.commitment.clone();e.last=Instant::now();e.device.state=State::Discovered;
                                                                     let mut offer=Message::new("OFFER",&m.device_id,m.request_id);offer.nonce=m.nonce;offer.audio_port=config.audio;offer.control_port=config.control;send(&socket,&offer,target).await;
                                                                     if let Some(code)=e.code.clone(){request=request.wrapping_add(1);let pair=pair_message(e,code,request,config.audio,config.control);send(&socket,&pair,e.peer).await;e.pending=Some(Pending{messages:vec![pair],position:0,reply:None,sent:Instant::now(),retries:0});}
                                                                    },
                                                                    Ok((n,peer))=socket.recv_from(&mut cb)=>{
                                        if let Ok(d)=serde_json::from_slice::<mic_protocol::diagnostics::Diagnostic>(&cb[..n]) {
                                            if let Some(e)=entries.get_mut(&d.device_id) && peer.ip()==e.peer.ip() && d.session_id==e.session && d.verify(&e.key) && e.tx.as_ref().is_none_or(|(_,_,sequence)|d.sequence>*sequence) {e.tx=Some((d.timing,Instant::now(),d.sequence));}continue;
                                        }

                                                                     let Ok(m)=Message::decode(&cb[..n]) else{continue};let Some(e)=entries.get_mut(&m.device_id)else{continue};
                                                                     if peer!=e.peer || m.session!=e.session || !m.verify(&e.key){continue};e.last=Instant::now();
                                                                     if m.kind=="HEARTBEAT" {continue}
                                                                     let Some(mut pending)=e.pending.take() else{continue};let current=&pending.messages[pending.position];if m.request_id!=current.request_id{e.pending=Some(pending);continue}
                                                                     if m.kind=="PAIR_REJECT" || m.kind=="ERROR" {let pairing=current.kind=="PAIR_REQUEST";let lab=e.lab.status=="requested";e.pending=Some(pending);fail(e,&m.error);if pairing {e.device.state=State::Discovered;e.code=None;}
                if lab {engine.playback.diagnostics.emit(mic_audio::diagnostics::EventType::TransportConfigRejected,None,None,engine.jitter.occupancy(),engine.jitter.depth,None,"tx_rejected_configuration");}continue}
                                                                     if (current.kind=="PAIR_REQUEST" && m.kind!="PAIR_ACCEPT") || (current.kind!="PAIR_REQUEST" && m.kind!="ACK"){e.pending=Some(pending);continue}
                                                                     if current.transport.is_some() && (m.transport!=current.transport || m.transport_stream!=current.transport_stream) {e.pending=Some(pending);engine.playback.diagnostics.emit(mic_audio::diagnostics::EventType::TransportConfigRejected,None,None,engine.jitter.occupancy(),engine.jitter.depth,None,"configuration_ack_mismatch");fail(e,"configuration ACK mismatch");continue}
                                                                     match current.kind.as_str(){"PAIR_REQUEST"=>{
                                        if e.accepted_session.as_ref()!=Some(&e.session) {e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::SessionChanged);}
                                        if e.accepted_session.is_some(){telemetry.event(&e.device.id,Some(e.session.clone()),EventType::Reconnect);}
                                        e.lab=LabState::default();e.tx=None;e.accepted_session=Some(e.session.clone());e.device.state=State::Connected;e.device.last_error=None;
                                                             for kind in ["SET_AUDIO_SOURCE","SET_GAIN","SET_MUTE"] {request=request.wrapping_add(1);let mut restore=control(e,kind,request);match kind {"SET_AUDIO_SOURCE"=>restore.source=Some(e.device.source.clone()),"SET_GAIN"=>restore.gain=Some(e.device.gain),_=>restore.mute=Some(e.device.mute)}restore.sign(&e.key);pending.messages.push(restore);}
                                                             if e.desired && active.as_ref().is_none_or(|id|id==&e.device.id) {active=Some(e.device.id.clone());request=request.wrapping_add(1);pending.messages.push(control(e,"START_STREAM",request));}},"SET_TRANSPORT"=>{e.lab.applied=current.transport;e.lab.error=None;engine.playback.diagnostics.emit(mic_audio::diagnostics::EventType::TransportConfigApplied,None,None,0,current.transport.unwrap().depth(),None,"tx_ack_stopped_configuration");},"START_STREAM"=>{e.lab.stream=current.transport_stream.unwrap_or(0);let cfg=e.lab.applied.unwrap_or(TransportConfig{jitter_buffer_target_ms:(config.jitter.clamp(2,4)*10) as u16,..TransportConfig::default()});engine.configure_transport(cfg,e.lab.applied.is_some());fec.clear();tick=time::interval(Duration::from_millis(cfg.frame_duration_ms as u64));tick.set_missed_tick_behavior(time::MissedTickBehavior::Burst);e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::StartStream);audio_baseline=audio_counters(&engine);e.device.state=State::Streaming;e.desired=true;active=Some(e.device.id.clone());engine.set_context(&e.device.id,&e.session);tick.reset();engine.set_mute(e.device.mute);},"STOP_STREAM"=>{account_audio(&engine,e,&mut audio_baseline);e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::StopStream);e.device.state=State::Connected;e.desired=false;if active.as_ref()==Some(&e.device.id){active=None;engine.reset();}},"SET_AUDIO_SOURCE"=>e.device.source=current.source.clone().unwrap(),"SET_GAIN"=>e.device.gain=current.gain.unwrap(),"SET_MUTE"=>{e.device.mute=current.mute.unwrap();if active.as_ref()==Some(&e.device.id){engine.set_mute(e.device.mute);}},_=>{}}
                                                                     e.last_request=m.request_id;pending.position+=1;
                                                                     if pending.position==pending.messages.len(){if e.lab.status=="requested"{e.lab.status="applied".into();}
                if let Some(reply)=pending.reply.take(){let _=reply.send(Ok(()));}}
                                                                     else {pending.sent=Instant::now();pending.retries=0;send(&socket,&pending.messages[pending.position],e.peer).await;e.pending=Some(pending);}
                                                                    },
                                                                    Ok((n,peer))=audio.recv_from(&mut ab)=>{
                         let arrival_us=engine.playback.diagnostics.now_us();
                         engine.jitter.metrics.received_datagrams+=1;engine.jitter.metrics.received_datagram_bytes+=n as u64;
                         if let Some(id)=&active && let Some(e)=entries.get_mut(id) {
                          if peer.ip()!=e.peer.ip(){engine.jitter.metrics.rejected_packets+=1;engine.jitter.metrics.session_rejections+=1;continue}
                          let decoded = if let Some(cfg)=e.lab.applied {LabPacket::decode(&ab[..n],&e.key,cfg,e.lab.stream).map(Some)} else {AudioPacket::decode(&ab[..n],&e.key).map(|audio|Some(LabPacket{audio,parity:false,stream:0,samples:160}))};
                          match decoded {
                           Ok(Some(p)) if hex::encode(p.audio.session)==e.session && e.device.state==State::Streaming=>{
                             e.last=Instant::now();
                             let recovered=if e.lab.applied.is_some_and(|c|c.fec.enabled){fec.receive(&p,arrival_us,engine.jitter.expected_sequence(),engine.jitter.started)}else{None};
                             if !p.parity {engine.receive_at(p.audio.clone(),arrival_us);}
                             if let Some(r)=recovered {engine.jitter.push(r);}
                             emit_fec(&engine,&mut fec);account_audio(&engine,e,&mut audio_baseline)},
                           Ok(_)=>{engine.jitter.metrics.rejected_packets+=1;engine.jitter.metrics.session_rejections+=1;},
                           Err(error)=>{engine.jitter.metrics.rejected_packets+=1;engine.jitter.metrics.invalid_packets+=1;if error.to_string().contains("authentication"){engine.jitter.metrics.authentication_failures+=1;}else{engine.jitter.metrics.invalid_packet_headers+=1;}}
                          }
                         }else {engine.jitter.metrics.unpaired_datagrams+=1;}
                        },
                        Some(command)=commands.recv()=>{
                                                                     let Some(e)=entries.get_mut(&command.id)else{let _=command.reply.send(Err("device not found".into()));continue};if e.pending.is_some(){let _=command.reply.send(Err("device command in progress".into()));continue}
                                                                     request=request.wrapping_add(1);let mut messages=vec![];
                                                                     match command.action {
                                                                      Action::Pair(code)=>{if e.device.state!=State::Discovered || code.len()!=32 || hex::decode(&code).is_err() || commitment(&code,&e.nonce)!=e.commitment{let _=command.reply.send(Err("invalid pairing credential or state".into()));continue}messages.push(pair_message(e,code,request,config.audio,config.control));},
                                                                      Action::Transport(cfg)=>{
                                                                       if let Err(err)=cfg.validate(){let _=command.reply.send(Err(err.to_string()));continue}
                                                                       if !matches!(e.device.state,State::Connected|State::Streaming) || !e.device.capabilities.iter().any(|c|c==CAPABILITY) {let _=command.reply.send(Err("device does not support Audio Lab or is not connected".into()));continue}
                                                                       if active.as_ref().is_some_and(|id|id!=&e.device.id){let _=command.reply.send(Err("another device is streaming".into()));continue}
                                                                       let resume=e.device.state==State::Streaming;engine.set_context(&e.device.id,&e.session);e.lab.requested=Some(cfg);e.lab.status="requested".into();e.lab.error=None;
                                                                       messages.push(control(e,"STOP_STREAM",request));request=request.wrapping_add(1);
                                                                       let mut m=control(e,"SET_TRANSPORT",request);m.transport=Some(cfg);m.transport_stream=Some(0);m.sign(&e.key);messages.push(m);
                                                                       if resume {request=request.wrapping_add(1);let mut m=control(e,"START_STREAM",request);m.transport=Some(cfg);m.transport_stream=Some(request);m.sign(&e.key);messages.push(m);}
                                                                      },
                                                                      Action::Reject=>{e.rejected_nonce=Some(e.nonce.clone());let m=Message::new("PAIR_REJECT",&e.device.id,request);send(&socket,&m,e.peer).await;e.device.state=State::Offline;e.code=None;let _=command.reply.send(Ok(()));continue},
                                                                      Action::Disconnect=>{if active.as_ref()==Some(&e.device.id){account_audio(&engine,e,&mut audio_baseline);}
                                        e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()).filter(|s|!s.is_empty()),EventType::Disconnect);
                                        if !e.session.is_empty(){let m=control(e,"STOP_STREAM",request);send(&socket,&m,e.peer).await;}e.device.state=State::Offline;e.code=None;e.desired=false;e.session.clear();if active.as_ref()==Some(&e.device.id){active=None;engine.reset();}let _=command.reply.send(Ok(()));continue},
                                                                      Action::Audio{source,gain,mute,streaming}=>{
                                                                       if !matches!(e.device.state,State::Connected|State::Streaming){let _=command.reply.send(Err("device is not connected".into()));continue}
                                                                       if streaming==Some(true) && active.as_ref().is_some_and(|id|id!=&e.device.id){let _=command.reply.send(Err("another device is streaming".into()));continue}
                                                                       if let Some(v)=source {let mut m=control(e,"SET_AUDIO_SOURCE",request);m.source=Some(v);m.sign(&e.key);messages.push(m);request=request.wrapping_add(1);}
                                                                       if let Some(v)=gain{let mut m=control(e,"SET_GAIN",request);m.gain=Some(v);m.sign(&e.key);messages.push(m);request=request.wrapping_add(1);}
                                                                       if let Some(v)=mute{let mut m=control(e,"SET_MUTE",request);m.mute=Some(v);m.sign(&e.key);messages.push(m);request=request.wrapping_add(1);}
                                                                       if let Some(v)=streaming{if v && e.lab.status=="failed" {let _=command.reply.send(Err("configuration uncertain; reapply transport first".into()));continue}
        if v {active=Some(e.device.id.clone());}let mut m=control(e,if v{"START_STREAM"}else{"STOP_STREAM"},request);if v && e.lab.applied.is_some() {m.transport=e.lab.applied;m.transport_stream=Some(request);m.sign(&e.key);}
                if v && e.lab.status=="failed" {let _=command.reply.send(Err("configuration uncertain; reapply transport first".into()));continue}messages.push(m);}
                                                                      }
                                                                     }
                                                                     if messages.is_empty(){let _=command.reply.send(Err("empty audio update".into()));continue}
                                                                     send(&socket,&messages[0],e.peer).await;e.pending=Some(Pending{messages,position:0,reply:Some(command.reply),sent:Instant::now(),retries:0});
                                                                    },
                                                                    deadline=tick.tick()=>{let overdue=time::Instant::now().saturating_duration_since(deadline);
                                if engine.playback.diagnostics.drift_telemetry_enabled() {
                                    let mut o=engine.playback.diagnostics.observation("receiver_tick");
                                    o.overdue_us=Some(overdue.as_micros() as u64);
                                    o.scheduled_us=Some(o.rx_monotonic_us.saturating_sub(overdue.as_micros() as u64));
                                    engine.playback.diagnostics.observe(o);
                                }
                                if overdue>Duration::from_millis(20){tick.reset();engine.rebuffer();engine.jitter.metrics.receiver_clock_resets+=1;if let Some(id)=&active && let Some(e)=entries.get(id){telemetry.event_with_context(&e.device.id,Some(e.session.clone()),EventType::BufferResync,None,Some(engine.jitter.metrics.jitter_buffer_depth),Some(engine.jitter.depth));}continue;}
                                fec.expire(engine.playback.diagnostics.now_us());emit_fec(&engine,&mut fec);
                                if let Some(e) = active.as_ref().and_then(|id| entries.get_mut(id)) && e.device.state == State::Streaming {
                                    let prev_underruns = engine.jitter.metrics.buffer_underruns;
                                    let prev_overruns = engine.jitter.metrics.buffer_overruns;
                                    let prev_lost = engine.jitter.metrics.lost_packets;
                                    engine.tick();
                                    fec.advance_deadline(engine.jitter.expected_sequence());
                                    account_audio(&engine, e, &mut audio_baseline);
                                    if engine.jitter.metrics.buffer_underruns > prev_underruns && prev_underruns.is_multiple_of(10) {
                                        telemetry.event_with_context(&e.device.id, Some(e.session.clone()), EventType::BufferUnderrun, None, Some(engine.jitter.metrics.jitter_buffer_depth), Some(engine.jitter.depth));
                                    }
                                    if engine.jitter.metrics.buffer_overruns > prev_overruns {
                                        telemetry.event_with_context(&e.device.id, Some(e.session.clone()), EventType::BufferOverrun, None, Some(engine.jitter.metrics.jitter_buffer_depth), Some(engine.jitter.depth));
                                    }
                                    if engine.jitter.metrics.lost_packets > prev_lost && prev_lost.is_multiple_of(10) {
                                        telemetry.event_with_context(&e.device.id, Some(e.session.clone()), EventType::DeadlineMiss, None, Some(engine.jitter.metrics.jitter_buffer_depth), Some(engine.jitter.depth));
                                    }
                                }
                            },
                                                                    _=maintenance.tick()=>{
                                                                     let now=Instant::now();let heartbeat_due=heartbeat.elapsed()>=Duration::from_secs(1);if heartbeat_due{heartbeat=now;}
                                                                     for e in entries.values_mut(){
                                                                      if e.pending.as_ref().is_some_and(|p|p.sent.elapsed()>Duration::from_millis(500)){
                                                                       if e.pending.as_ref().unwrap().retries>=3{let lab=e.lab.status=="requested";if lab {engine.playback.diagnostics.emit(mic_audio::diagnostics::EventType::TransportConfigRejected,None,None,engine.jitter.occupancy(),engine.jitter.depth,None,"configuration_control_timeout_uncertain");}fail(e,"control timeout after 4 attempts");if lab && active.as_ref()==Some(&e.device.id){active=None;engine.reset();e.device.state=State::Connected;}
                if active.as_ref()==Some(&e.device.id) && e.device.state!=State::Streaming {active=None;engine.reset();}
                                                      if e.device.state==State::Pairing{e.device.state=State::Discovered;e.code=None;}}
                                                                       else if let Some(p)=&mut e.pending{p.retries+=1;p.sent=now;send(&socket,&p.messages[p.position],e.peer).await;}
                                                                      }
                                                                      if e.last.elapsed()>Duration::from_secs(5) && e.device.state!=State::Offline {if active.as_ref()==Some(&e.device.id){account_audio(&engine,e,&mut audio_baseline);}
                                        e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()).filter(|s|!s.is_empty()),EventType::Disconnect);
                                        fail(e,"session timeout");e.device.state=State::Offline;if active.as_ref()==Some(&e.device.id){active=None;engine.reset();}}
                                                                      if heartbeat_due && matches!(e.device.state,State::Connected|State::Streaming){request=request.wrapping_add(1);let h=control(e,"HEARTBEAT",request);send(&socket,&h,e.peer).await;}
                                                                     }
                                                                     telemetry.publish(entries.values().map(|e|DeviceInput{device_id:e.device.id.clone(),session_id:Some(e.session.clone()).filter(|s|!s.is_empty()),connection_state:e.device.state,counters:e.counters,jitter_buffer_depth:if active.as_ref()==Some(&e.device.id){engine.jitter.metrics.jitter_buffer_depth}else{0},activity_epoch:e.activity_epoch,counter_timestamp_ms:Some(counter_clock.elapsed().as_millis() as u64),tx:e.tx.as_ref().filter(|(_,updated,_)|updated.elapsed()<Duration::from_millis(2500)).map(|(t,_,_)|t.clone()),rx_inter_arrival_jitter_ms:if active.as_ref()==Some(&e.device.id) && e.device.state==State::Streaming{Some(engine.metrics().inter_arrival_jitter_ms)}else{None},sequence_gaps:if active.as_ref()==Some(&e.device.id){engine.jitter.metrics.sequence_gaps}else{0},late_packet_drops:if active.as_ref()==Some(&e.device.id){engine.jitter.metrics.late_packet_drops}else{0},target_buffer_depth:if active.as_ref()==Some(&e.device.id){engine.jitter.depth}else{0},underrun_fade_outs:if active.as_ref()==Some(&e.device.id){engine.declicker.fade_outs}else{0},recovery_fade_ins:if active.as_ref()==Some(&e.device.id){engine.declicker.fade_ins}else{0}}).collect());
                                        let mut snap=snapshot.write().await;snap.devices=entries.values().map(|e|e.device.clone()).collect();snap.devices.sort_by(|a,b|a.id.cmp(&b.id));snap.metrics=engine.metrics();snap.active_device=active.clone();snap.lab=entries.iter().map(|(id,e)|(id.clone(),e.lab.clone())).collect();snap.fec=fec.metrics;snap.counter_monotonic_us=engine.playback.diagnostics.now_us();snap.diagnostic_stream_id=engine.playback.diagnostics.current_stream_id();
                                                                    }
                                                                    }
    }
    for e in entries.values_mut() {
        fail(e, "service shutting down");
        if !e.session.is_empty() {
            request = request.wrapping_add(1);
            send(&socket, &control(e, "STOP_STREAM", request), e.peer).await;
        }
    }
    engine.reset();
    engine.playback.shutdown();
}
pub fn validate_audio(gain: Option<f32>) -> Result<()> {
    if gain.is_some_and(|g| !g.is_finite() || !(0.0..=1.0).contains(&g)) {
        return Err(anyhow!("gain must be between 0 and 1"));
    }
    Ok(())
}

fn audio_counters(engine: &Engine) -> Counters {
    let metrics = &engine.jitter.metrics;
    Counters {
        received_packets: engine.authenticated_audio_packets(),
        lost_packets: metrics.lost_packets,
        out_of_order_packets: metrics.sequence_out_of_order,
        received_bytes: engine.authenticated_pcm_bytes(),
        buffer_underruns: metrics.buffer_underruns.saturating_add(
            engine
                .playback
                .underruns
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
        buffer_overruns: metrics.buffer_overruns.saturating_add(
            engine
                .playback
                .overruns
                .load(std::sync::atomic::Ordering::Relaxed),
        ),
    }
}
fn account_audio(engine: &Engine, entry: &mut Entry, baseline: &mut Counters) {
    let current = audio_counters(engine);
    if let Some(delta) = current.checked_delta(*baseline) {
        entry.counters.add(delta);
    }
    *baseline = current;
}

#[cfg(test)]
mod telemetry_isolation_tests {
    use super::*;
    use std::sync::Mutex;
    #[tokio::test]
    async fn history_write_lock_cannot_block_audio_receiver() {
        let reserved: Vec<_> = (0..3)
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .collect();
        let ports: Vec<_> = reserved
            .iter()
            .map(|s| s.local_addr().unwrap().port())
            .collect();
        drop(reserved);
        let config = Config {
            discovery: ports[0],
            control: ports[1],
            audio: ports[2],
            jitter: 3,
        };
        let (core, actor, stop) = Core::start(config).await.unwrap();
        let tx = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let code = "0123456789abcdef0123456789abcdef";
        let nonce = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut discover = Message::new("DISCOVER", "isolation-tx", 1);
        discover.nonce = nonce.into();
        discover.commitment = commitment(code, nonce);
        discover.control_port = tx.local_addr().unwrap().port();
        tx.send_to(
            &serde_json::to_vec(&discover).unwrap(),
            ("127.0.0.1", ports[0]),
        )
        .await
        .unwrap();
        let material = Arc::new(Mutex::new(None::<([u8; 32], [u8; 16])>));
        let shared = material.clone();
        let socket = tx.clone();
        let driver = tokio::spawn(async move {
            let mut buffer = [0; 2049];
            let mut key = [0; 32];
            loop {
                let (n, peer) = socket.recv_from(&mut buffer).await.unwrap();
                let m = Message::decode(&buffer[..n]).unwrap();
                if m.kind == "OFFER" {
                    continue;
                }
                let kind = if m.kind == "PAIR_REQUEST" {
                    assert!(m.verify(code.as_bytes()));
                    key = session_key(code, &m.nonce, &m.session);
                    *shared.lock().unwrap() =
                        Some((key, hex::decode(&m.session).unwrap().try_into().unwrap()));
                    "PAIR_ACCEPT"
                } else if m.kind == "HEARTBEAT" {
                    "HEARTBEAT"
                } else {
                    "ACK"
                };
                let mut reply = Message::new(kind, &m.device_id, m.request_id);
                reply.session = m.session;
                reply.sign(&key);
                socket
                    .send_to(&serde_json::to_vec(&reply).unwrap(), peer)
                    .await
                    .unwrap();
            }
        });
        time::timeout(Duration::from_secs(2), async {
            loop {
                if core
                    .snapshot
                    .read()
                    .await
                    .devices
                    .iter()
                    .any(|d| d.id == "isolation-tx")
                {
                    break;
                }
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        core.command("isolation-tx".into(), Action::Pair(code.into()))
            .await
            .unwrap();
        core.command(
            "isolation-tx".into(),
            Action::Audio {
                source: None,
                gain: None,
                mute: None,
                streaming: Some(true),
            },
        )
        .await
        .unwrap();
        let (key, session) = material.lock().unwrap().unwrap();
        // Deliberately stall both sampling and history readers. Audio must still progress.
        let history_lock = core.telemetry.store.write().await;
        for sequence in 0..120 {
            let p = AudioPacket {
                sequence,
                timestamp: sequence * 160,
                session,
                samples: [0; 160],
            };
            tx.send_to(&p.encode(&key), ("127.0.0.1", ports[2]))
                .await
                .unwrap();
            time::sleep(Duration::from_millis(10)).await;
        }
        time::sleep(Duration::from_millis(300)).await;
        assert!(core.snapshot.read().await.metrics.received_packets >= 110);
        drop(history_lock);
        let _ = stop.send(());
        time::timeout(Duration::from_secs(2), actor)
            .await
            .unwrap()
            .unwrap();
        driver.abort();
        let _ = driver.await;
    }
}
