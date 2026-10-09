use anyhow::Result;
use mic_protocol::*;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, time};
fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.windows(2)
        .find(|v| v[0] == name)
        .map(|v| v[1].clone())
        .unwrap_or(default.into())
}
#[tokio::main]
async fn main() -> Result<()> {
    let _timer = mic_protocol::TimerResolution::acquire();
    let id = arg("--id", "sim-01");
    let discovery: SocketAddr = arg("--discovery", "127.0.0.1:41000").parse()?;
    let bind = arg("--bind", "127.0.0.1:0");
    let loss: u32 = arg("--loss-every", "0").parse()?;
    let reorder: u32 = arg("--reorder-every", "0").parse()?;
    let jitter: u64 = arg("--jitter-ms", "0").parse()?;
    let reconnect: u64 = arg("--reconnect-after", "0").parse()?;
    let code = arg("--code", &hex_code());
    if code.len() != 32
        || !code
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        anyhow::bail!("code must be 32 hexadecimal characters")
    };
    println!("Pairing credential for {id}: {code}");
    let socket = UdpSocket::bind(bind).await?;
    let control_port = socket.local_addr()?.port();
    let mut nonce = hex_code();
    let mut session = String::new();
    let mut key = [0u8; 32];
    let mut peer: Option<SocketAddr> = None;
    let mut audio_peer = None;
    let mut streaming = false;
    let mut generator = Generator::default();
    let mut gain = 1.0;
    let mut mute = false;
    let mut seq = 0u32;
    let mut request = 0u32;
    let mut cached: VecDeque<(u32, Message)> = VecDeque::new();
    let mut held = None;
    let mut last = Instant::now();
    let started = Instant::now();
    let mut reconnected = false;
    let mut delayed: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
    let mut frame = time::interval(Duration::from_millis(10));
    frame.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut discover = time::interval(Duration::from_secs(1));
    let mut buf = [0u8; 2049];
    loop {
        tokio::select! {
               _=tokio::signal::ctrl_c()=>break,
               _=discover.tick()=>{
                if reconnect>0 && !reconnected && started.elapsed().as_secs()>=reconnect {session.clear();peer=None;streaming=false;nonce=hex_code();cached.clear();reconnected=true;println!("Simulated reconnect");}
                if !session.is_empty() && last.elapsed()>Duration::from_secs(5){session.clear();peer=None;streaming=false;cached.clear();nonce=hex_code();}
                request=request.wrapping_add(1);
                if session.is_empty(){let mut m=Message::new("DISCOVER",&id,request);m.name="軟體測試發射器".into();m.device_type="simulator".into();m.capabilities=vec!["sine".into(),"sweep".into(),"pulse".into(),"silence".into()];m.nonce=nonce.clone();m.commitment=commitment(&code,&nonce);m.control_port=control_port;socket.send_to(&serde_json::to_vec(&m)?,discovery).await?;}
                else if let Some(p)=peer{let mut m=Message::new("HEARTBEAT",&id,request);m.session=session.clone();m.sign(&key);socket.send_to(&serde_json::to_vec(&m)?,p).await?;}
               },
               Ok((n,from))=socket.recv_from(&mut buf)=>{
                let Ok(m)=Message::decode(&buf[..n])else{continue};if m.device_id!=id{continue}
                if m.kind=="OFFER" && m.nonce==nonce {continue}
                if m.kind=="PAIR_REQUEST" {if m.nonce!=nonce || m.session.len()!=32 || !m.verify(code.as_bytes()){continue}
        if !session.is_empty() && (session!=m.session || peer!=Some(from)){continue}session=m.session.clone();key=session_key(&code,&nonce,&session);peer=Some(from);audio_peer=Some(SocketAddr::new(from.ip(),m.audio_port));if !streaming {seq=0;generator.sample=0;}last=Instant::now();let mut reply=Message::new("PAIR_ACCEPT",&id,m.request_id);reply.session=session.clone();reply.sign(&key);socket.send_to(&serde_json::to_vec(&reply)?,from).await?;println!("Paired session {session}");continue}
                if peer!=Some(from)||m.session!=session||!m.verify(&key){continue}last=Instant::now();if m.kind=="HEARTBEAT" {continue}
                if let Some((_,reply))=cached.iter().find(|(id,_)|*id==m.request_id){socket.send_to(&serde_json::to_vec(reply)?,from).await?;continue}
                let mut reply=Message::new("ACK",&id,m.request_id);reply.session=session.clone();match m.kind.as_str(){"START_STREAM"=>{streaming=true;seq=0;generator.sample=0;delayed.clear();held=None;},"STOP_STREAM"=>{streaming=false;delayed.clear();held=None;},"SET_AUDIO_SOURCE"=>{if let Some(s)=m.source{generator.source=s;}},"SET_GAIN"=>gain=m.gain.unwrap_or(gain),"SET_MUTE"=>mute=m.mute.unwrap_or(mute),_=>{reply.kind="ERROR".into();reply.error="unsupported command".into();}}
                reply.sign(&key);socket.send_to(&serde_json::to_vec(&reply)?,from).await?;cached.push_back((m.request_id,reply));if cached.len()>32{cached.pop_front();}
               },
               _=frame.tick()=>{if streaming{let p=AudioPacket{sequence:seq,timestamp:generator.sample as u32,session:hex_session(&session),samples:generator.frame(gain,mute)};seq=seq.wrapping_add(1);if loss==0||!seq.is_multiple_of(loss) {let bytes=p.encode(&key);let due=Instant::now()+Duration::from_millis(if jitter==0{0}else{rand::random::<u64>()%(jitter+1)});if reorder>0 && seq.is_multiple_of(reorder) {held=Some((due,bytes));}else{if delayed.len()<16{delayed.push_back((due,bytes));}
        if let Some(h)=held.take()&& delayed.len()<16{delayed.push_back(h);}}}
        if let Some(p)=audio_peer{while delayed.front().is_some_and(|(due,_)|*due<=Instant::now()){let(_,b)=delayed.pop_front().unwrap();socket.send_to(&b,p).await?;}}}},
               }
    }
    Ok(())
}
fn hex_code() -> String {
    rand::random::<[u8; 16]>()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}
fn hex_session(s: &str) -> [u8; 16] {
    std::array::from_fn(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
}
