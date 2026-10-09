use anyhow::{Result, anyhow};
use mic_audio::{Engine, Metrics, Playback};
use mic_device::{Device, State};
use mic_protocol::{AudioPacket, Message, Source, commitment, session_key};
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
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub metrics: Metrics,
    pub active_device: Option<String>,
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
        let engine = Engine::new(config.jitter);
        let playback = engine.playback.clone();
        let snapshot = Arc::new(RwLock::new(Snapshot {
            devices: vec![],
            metrics: engine.metrics(),
            active_device: None,
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
    loop {
        tokio::select! {
                                                    _=&mut stopped=>break,
                                                    Ok((n,peer))=discovery.recv_from(&mut db)=>{
                                                     let Ok(m)=Message::decode(&db[..n]) else{continue};if m.kind!="DISCOVER" || m.nonce.len()!=32 || m.commitment.len()!=64 || m.control_port==0 || hex::decode(&m.nonce).is_err() || hex::decode(&m.commitment).is_err(){continue}
                                                     if !entries.contains_key(&m.device_id) && entries.len()>=64 {continue}
                                                     let target=SocketAddr::new(peer.ip(),m.control_port);
                                                     let e=entries.entry(m.device_id.clone()).or_insert_with(||Entry{device:Device{id:m.device_id.clone(),name:m.name.clone(),device_type:m.device_type.clone(),capabilities:m.capabilities.clone(),address:target.to_string(),state:State::Discovered,source:Source::Sine,gain:1.0,mute:false,last_error:None},peer:target,nonce:m.nonce.clone(),commitment:m.commitment.clone(),code:None,key:[0;32],session:String::new(),last:Instant::now(),pending:None,desired:false,last_request:0,rejected_nonce:None,counters:Counters::default(),activity_epoch:0,accepted_session:None,tx:None});
                                                     if e.rejected_nonce.as_ref()==Some(&m.nonce) {continue}
                               if matches!(e.device.state,State::Connected|State::Streaming|State::Pairing){continue}
                                                     // Reconnecting a known identity requires the previous credential to verify the fresh commitment.
                                                     if let Some(code)=&e.code&& commitment(code,&m.nonce)!=m.commitment {continue}
                                                     e.rejected_nonce=None;e.peer=target;e.device.address=target.to_string();e.nonce=m.nonce.clone();e.commitment=m.commitment.clone();e.last=Instant::now();e.device.state=State::Discovered;
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
                                                     if m.kind=="PAIR_REJECT" || m.kind=="ERROR" {e.pending=Some(pending);fail(e,&m.error);e.device.state=State::Discovered;e.code=None;continue}
                                                     if (current.kind=="PAIR_REQUEST" && m.kind!="PAIR_ACCEPT") || (current.kind!="PAIR_REQUEST" && m.kind!="ACK"){e.pending=Some(pending);continue}
                                                     match current.kind.as_str(){"PAIR_REQUEST"=>{
                        if e.accepted_session.as_ref()!=Some(&e.session) {e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::SessionChanged);}
                        if e.accepted_session.is_some(){telemetry.event(&e.device.id,Some(e.session.clone()),EventType::Reconnect);}
                        e.tx=None;e.accepted_session=Some(e.session.clone());e.device.state=State::Connected;e.device.last_error=None;
                                             for kind in ["SET_AUDIO_SOURCE","SET_GAIN","SET_MUTE"] {request=request.wrapping_add(1);let mut restore=control(e,kind,request);match kind {"SET_AUDIO_SOURCE"=>restore.source=Some(e.device.source.clone()),"SET_GAIN"=>restore.gain=Some(e.device.gain),_=>restore.mute=Some(e.device.mute)}restore.sign(&e.key);pending.messages.push(restore);}
                                             if e.desired && active.as_ref().is_none_or(|id|id==&e.device.id) {active=Some(e.device.id.clone());request=request.wrapping_add(1);pending.messages.push(control(e,"START_STREAM",request));}},"START_STREAM"=>{e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::StartStream);audio_baseline=audio_counters(&engine);e.device.state=State::Streaming;e.desired=true;active=Some(e.device.id.clone());engine.reset();tick.reset();engine.set_mute(e.device.mute);},"STOP_STREAM"=>{account_audio(&engine,e,&mut audio_baseline);e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()),EventType::StopStream);e.device.state=State::Connected;e.desired=false;if active.as_ref()==Some(&e.device.id){active=None;engine.reset();}},"SET_AUDIO_SOURCE"=>e.device.source=current.source.clone().unwrap(),"SET_GAIN"=>e.device.gain=current.gain.unwrap(),"SET_MUTE"=>{e.device.mute=current.mute.unwrap();if active.as_ref()==Some(&e.device.id){engine.set_mute(e.device.mute);}},_=>{}}
                                                     e.last_request=m.request_id;pending.position+=1;
                                                     if pending.position==pending.messages.len(){if let Some(reply)=pending.reply.take(){let _=reply.send(Ok(()));}}
                                                     else {pending.sent=Instant::now();pending.retries=0;send(&socket,&pending.messages[pending.position],e.peer).await;e.pending=Some(pending);}
                                                    },
                                                    Ok((n,peer))=audio.recv_from(&mut ab)=>{
         engine.jitter.metrics.received_datagrams+=1;
         if let Some(id)=&active && let Some(e)=entries.get_mut(id) {
          if peer.ip()!=e.peer.ip(){engine.jitter.metrics.rejected_packets+=1;engine.jitter.metrics.session_rejections+=1;continue}
          match AudioPacket::decode(&ab[..n],&e.key) {
           Ok(p) if hex::encode(p.session)==e.session && e.device.state==State::Streaming=>{e.last=Instant::now();engine.receive(p);account_audio(&engine,e,&mut audio_baseline)},
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
                                                       if let Some(v)=streaming{if v {active=Some(e.device.id.clone());}messages.push(control(e,if v{"START_STREAM"}else{"STOP_STREAM"},request));}
                                                      }
                                                     }
                                                     if messages.is_empty(){let _=command.reply.send(Err("empty audio update".into()));continue}
                                                     send(&socket,&messages[0],e.peer).await;e.pending=Some(Pending{messages,position:0,reply:Some(command.reply),sent:Instant::now(),retries:0});
                                                    },
                                                    deadline=tick.tick()=>{if time::Instant::now().saturating_duration_since(deadline)>Duration::from_millis(20){tick.reset();engine.rebuffer();engine.jitter.metrics.receiver_clock_resets+=1;continue;}
                if let Some(e)=active.as_ref().and_then(|id|entries.get_mut(id)) && e.device.state==State::Streaming {engine.tick();account_audio(&engine,e,&mut audio_baseline);}},
                                                    _=maintenance.tick()=>{
                                                     let now=Instant::now();let heartbeat_due=heartbeat.elapsed()>=Duration::from_secs(1);if heartbeat_due{heartbeat=now;}
                                                     for e in entries.values_mut(){
                                                      if e.pending.as_ref().is_some_and(|p|p.sent.elapsed()>Duration::from_millis(500)){
                                                       if e.pending.as_ref().unwrap().retries>=3{fail(e,"control timeout after 4 attempts");if active.as_ref()==Some(&e.device.id) && e.device.state!=State::Streaming {active=None;engine.reset();}
                                      if e.device.state==State::Pairing{e.device.state=State::Discovered;e.code=None;}}
                                                       else if let Some(p)=&mut e.pending{p.retries+=1;p.sent=now;send(&socket,&p.messages[p.position],e.peer).await;}
                                                      }
                                                      if e.last.elapsed()>Duration::from_secs(5) && e.device.state!=State::Offline {if active.as_ref()==Some(&e.device.id){account_audio(&engine,e,&mut audio_baseline);}
                        e.activity_epoch+=1;telemetry.event(&e.device.id,Some(e.session.clone()).filter(|s|!s.is_empty()),EventType::Disconnect);
                        fail(e,"session timeout");e.device.state=State::Offline;if active.as_ref()==Some(&e.device.id){active=None;engine.reset();}}
                                                      if heartbeat_due && matches!(e.device.state,State::Connected|State::Streaming){request=request.wrapping_add(1);let h=control(e,"HEARTBEAT",request);send(&socket,&h,e.peer).await;}
                                                     }
                                                     telemetry.publish(entries.values().map(|e|DeviceInput{device_id:e.device.id.clone(),session_id:Some(e.session.clone()).filter(|s|!s.is_empty()),connection_state:e.device.state,counters:e.counters,jitter_buffer_depth:if active.as_ref()==Some(&e.device.id){engine.jitter.metrics.jitter_buffer_depth}else{0},activity_epoch:e.activity_epoch,counter_timestamp_ms:Some(counter_clock.elapsed().as_millis() as u64),tx:e.tx.as_ref().filter(|(_,updated,_)|updated.elapsed()<Duration::from_millis(2500)).map(|(t,_,_)|t.clone())}).collect());
                        let mut snap=snapshot.write().await;snap.devices=entries.values().map(|e|e.device.clone()).collect();snap.devices.sort_by(|a,b|a.id.cmp(&b.id));snap.metrics=engine.metrics();snap.active_device=active.clone();
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
        received_packets: metrics.received_packets,
        lost_packets: metrics.lost_packets,
        out_of_order_packets: metrics.sequence_out_of_order,
        received_bytes: metrics.received_packets.saturating_mul(320),
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
