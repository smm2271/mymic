mod timing;
mod transmitter;
use anyhow::Result;
use mic_protocol::*;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, time};
use transmitter::{Options, Transmitter};
fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.windows(2)
        .find(|v| v[0] == name)
        .map(|v| v[1].clone())
        .unwrap_or(default.into())
}
#[tokio::main]
async fn main() -> Result<()> {
    let id = arg("--id", "sim-01");
    let discovery: SocketAddr = arg("--discovery", "127.0.0.1:41000").parse()?;
    let mode = timing::Mode::parse(&arg(
        "--timing-mode",
        if cfg!(windows) {
            "waitable"
        } else {
            "absolute"
        },
    ))?;
    let code = arg("--code", &hex::encode(rand::random::<[u8; 16]>()));
    if code.len() != 32
        || !code
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        anyhow::bail!("code must be 32 lowercase hexadecimal characters")
    }
    let diagnostic_path = std::env::args()
        .any(|a| a == "--timing-diagnostics")
        .then(|| arg("--diagnostics-file", "artifacts/simulator-timing.jsonl"));
    let mut transmitter = Transmitter::start(Options {
        mode,
        device_id: id.clone(),
        loss: arg("--loss-every", "0").parse()?,
        reorder: arg("--reorder-every", "0").parse()?,
        jitter_ms: arg("--jitter-ms", "0").parse()?,
        diagnostic_path,
    })?;
    println!("Pairing credential for {id}: {code}\nTiming mode: {mode:?}");
    let socket = UdpSocket::bind(arg("--bind", "127.0.0.1:0")).await?;
    let control_port = socket.local_addr()?.port();
    let reconnect: u64 = arg("--reconnect-after", "0").parse()?;
    let duration: u64 = arg("--run-seconds", "0").parse()?;
    let mut nonce = hex::encode(rand::random::<[u8; 16]>());
    let mut session = String::new();
    let mut key = [0; 32];
    let mut peer = None;
    let mut request = 0u32;
    let mut cached: VecDeque<(u32, Message)> = VecDeque::with_capacity(32);
    let mut last = Instant::now();
    let started = Instant::now();
    let mut reconnected = false;
    let mut discover = time::interval(Duration::from_secs(1));
    let mut buf = [0; 2049];
    loop {
        tokio::select! {
        _=tokio::signal::ctrl_c()=>break,
        _=discover.tick()=>{
         if duration>0&&started.elapsed().as_secs()>=duration{break}
         if (reconnect>0&&!reconnected&&started.elapsed().as_secs()>=reconnect)||(!session.is_empty()&&last.elapsed()>Duration::from_secs(5)) {session.clear();peer=None;cached.clear();nonce=hex::encode(rand::random::<[u8;16]>());transmitter.settings.lock().unwrap().running=false;reconnected=true;println!("Simulated reconnect");}
         request=request.wrapping_add(1);
         if session.is_empty(){let mut m=Message::new("DISCOVER",&id,request);m.name="軟體測試發射器".into();m.device_type="simulator".into();m.capabilities=vec!["sine".into(),"sweep".into(),"pulse".into(),"silence".into(),lab::CAPABILITY.into(),format!("tx_build:{}",env!("CARGO_PKG_VERSION"))];m.nonce=nonce.clone();m.commitment=commitment(&code,&nonce);m.control_port=control_port;socket.send_to(&serde_json::to_vec(&m)?,discovery).await?;}
         else if let Some(p)=peer{let mut m=Message::new("HEARTBEAT",&id,request);m.session=session.clone();m.sign(&key);socket.send_to(&serde_json::to_vec(&m)?,p).await?;}
        },
        Ok((n,from))=socket.recv_from(&mut buf)=>{
         let Ok(m)=Message::decode(&buf[..n])else{continue};if m.device_id!=id{continue}
         if m.kind=="OFFER"&&m.nonce==nonce{continue}
         if m.kind=="PAIR_REQUEST" {
          if m.nonce!=nonce||m.session.len()!=32||hex::decode(&m.session).is_err()||!m.verify(code.as_bytes())||(!session.is_empty()&&(session!=m.session||peer!=Some(from))){continue}
          let fresh=session!=m.session;session=m.session.clone();key=session_key(&code,&nonce,&session);peer=Some(from);last=Instant::now();
          {let mut state=transmitter.settings.lock().unwrap();if fresh {state.transport=None;state.stream=0;}state.session=hex::decode(&session)?.try_into().unwrap();state.key=key;state.peer=Some(SocketAddr::new(from.ip(),m.audio_port));state.diagnostic_peer=Some(from);}
          let mut reply=Message::new("PAIR_ACCEPT",&id,m.request_id);reply.session=session.clone();reply.sign(&key);socket.send_to(&serde_json::to_vec(&reply)?,from).await?;println!("Paired session {session}");continue;
         }
         if peer!=Some(from)||m.session!=session||!m.verify(&key){continue}last=Instant::now();if m.kind=="HEARTBEAT"{continue}
         if let Some((_,reply))=cached.iter().find(|(id,_)|*id==m.request_id){socket.send_to(&serde_json::to_vec(reply)?,from).await?;continue}
         let mut reply=Message::new("ACK",&id,m.request_id);reply.session=session.clone();
         {let mut state=transmitter.settings.lock().unwrap();match m.kind.as_str(){"SET_TRANSPORT"=>{if state.running || m.transport.is_none(){reply.kind="ERROR".into();reply.error="transport requires stopped stream".into();}else {state.transport=m.transport;reply.transport=m.transport;reply.transport_stream=m.transport_stream;}},"START_STREAM"=>{if state.transport!=m.transport {reply.kind="ERROR".into();reply.error="transport mismatch".into();}else {state.stream=m.transport_stream.unwrap_or(0);state.epoch+=1;state.running=true;reply.transport=m.transport;reply.transport_stream=m.transport_stream;}},"STOP_STREAM"=>state.running=false,"SET_AUDIO_SOURCE"=>{if let Some(source)=m.source{state.source=source;}},"SET_GAIN"=>state.gain=m.gain.unwrap_or(state.gain),"SET_MUTE"=>state.mute=m.mute.unwrap_or(state.mute),_=>{reply.kind="ERROR".into();reply.error="unsupported command".into();}}}
         reply.sign(&key);socket.send_to(&serde_json::to_vec(&reply)?,from).await?;cached.push_back((m.request_id,reply));if cached.len()>32{cached.pop_front();}
        }
        }
    }
    transmitter.shutdown()?;
    Ok(())
}
