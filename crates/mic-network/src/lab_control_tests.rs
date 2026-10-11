use super::*;
#[test]
fn raw_receive_population_and_payload_bytes_exclude_fec_reconstruction() {
    let mut engine = Engine::new(3);
    engine.configure_transport(
        TransportConfig {
            frame_duration_ms: 5,
            ..TransportConfig::default()
        },
        true,
    );
    let p = AudioPacket {
        sequence: 0,
        timestamp: 0,
        session: [1; 16],
        samples: [0; 160],
    };
    engine.receive(p.clone());
    let mut recovered = p;
    recovered.sequence = 1;
    recovered.timestamp = 80;
    engine.jitter.push(recovered);
    assert_eq!(engine.jitter.metrics.received_packets, 2);
    assert_eq!(audio_counters(&engine).received_packets, 1);
    assert_eq!(audio_counters(&engine).received_bytes, 160);
}
use std::sync::{
    Mutex,
    atomic::{AtomicU8, Ordering},
};
#[tokio::test]
async fn authenticated_config_ack_retry_failure_and_safe_start_boundary() {
    let reserved: Vec<_> = (0..3)
        .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = reserved
        .iter()
        .map(|s| s.local_addr().unwrap().port())
        .collect();
    drop(reserved);
    let (core, actor, stop) = Core::start(Config {
        discovery: ports[0],
        control: ports[1],
        audio: ports[2],
        jitter: 3,
    })
    .await
    .unwrap();
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let code = "0123456789abcdef0123456789abcdef";
    let nonce = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut d = Message::new("DISCOVER", "lab-control", 1);
    d.nonce = nonce.into();
    d.commitment = commitment(code, nonce);
    d.control_port = socket.local_addr().unwrap().port();
    d.capabilities = vec![CAPABILITY.into()];
    socket
        .send_to(&serde_json::to_vec(&d).unwrap(), ("127.0.0.1", ports[0]))
        .await
        .unwrap();
    let mode = Arc::new(AtomicU8::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    let control_socket = socket.clone();
    let driver_mode = mode.clone();
    let driver_log = log.clone();
    let driver = tokio::spawn(async move {
        let mut key = [0; 32];
        let mut buf = [0; 2049];
        loop {
            let (n, peer) = control_socket.recv_from(&mut buf).await.unwrap();
            let m = Message::decode(&buf[..n]).unwrap();
            if m.kind == "OFFER" {
                continue;
            }
            if m.kind == "PAIR_REQUEST" {
                assert!(m.verify(code.as_bytes()));
                key = session_key(code, &m.nonce, &m.session);
            } else {
                assert!(m.verify(&key));
            }
            if m.kind == "HEARTBEAT" {
                continue;
            }
            driver_log
                .lock()
                .unwrap()
                .push((m.kind.clone(), m.request_id));
            let flag = driver_mode.load(Ordering::Relaxed);
            if m.kind == "SET_TRANSPORT" && flag == 2 {
                driver_mode.store(0, Ordering::Relaxed);
                continue;
            }
            if m.kind == "SET_TRANSPORT" && flag == 4 {
                continue;
            }
            let mut r = Message::new(
                if m.kind == "PAIR_REQUEST" {
                    "PAIR_ACCEPT"
                } else {
                    "ACK"
                },
                &m.device_id,
                m.request_id,
            );
            r.session = m.session;
            r.transport = m.transport;
            r.transport_stream = m.transport_stream;
            if m.kind == "SET_TRANSPORT" && flag == 1 {
                r.transport.as_mut().unwrap().jitter_buffer_target_ms = 100;
            }
            if m.kind == "SET_TRANSPORT" && flag == 3 {
                r.kind = "ERROR".into();
                r.error = "test rejection".into();
            }
            r.sign(&key);
            control_socket
                .send_to(&serde_json::to_vec(&r).unwrap(), peer)
                .await
                .unwrap();
        }
    });
    time::timeout(Duration::from_secs(3), async {
        while core.snapshot.read().await.devices.is_empty() {
            time::sleep(Duration::from_millis(10)).await
        }
    })
    .await
    .unwrap();
    core.command("lab-control".into(), Action::Pair(code.into()))
        .await
        .unwrap();
    let cfg = TransportConfig {
        frame_duration_ms: 5,
        jitter_buffer_target_ms: 40,
        ..TransportConfig::default()
    };
    core.command(
        "lab-control".into(),
        Action::Audio {
            source: None,
            gain: None,
            mute: None,
            streaming: Some(true),
        },
    )
    .await
    .unwrap();
    mode.store(2, Ordering::Relaxed);
    core.command("lab-control".into(), Action::Transport(cfg))
        .await
        .unwrap();
    {
        let commands = log.lock().unwrap();
        let tail = &commands[commands.len() - 4..];
        assert_eq!(
            tail.iter().map(|v| v.0.as_str()).collect::<Vec<_>>(),
            [
                "STOP_STREAM",
                "SET_TRANSPORT",
                "SET_TRANSPORT",
                "START_STREAM"
            ]
        );
        assert_eq!(tail[1].1, tail[2].1);
    }
    mode.store(1, Ordering::Relaxed);
    assert!(
        core.command(
            "lab-control".into(),
            Action::Transport(TransportConfig::default())
        )
        .await
        .unwrap_err()
        .contains("ACK mismatch")
    );
    time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        core.snapshot.read().await.lab["lab-control"].applied,
        Some(cfg)
    );
    assert_eq!(
        core.snapshot.read().await.lab["lab-control"].status,
        "failed"
    );
    assert!(
        core.command(
            "lab-control".into(),
            Action::Audio {
                source: None,
                gain: None,
                mute: None,
                streaming: Some(true)
            }
        )
        .await
        .is_err()
    );
    for flag in [3, 4] {
        mode.store(flag, Ordering::Relaxed);
        assert!(
            core.command(
                "lab-control".into(),
                Action::Transport(TransportConfig::default())
            )
            .await
            .is_err()
        );
    }
    mode.store(0, Ordering::Relaxed);
    core.command(
        "lab-control".into(),
        Action::Transport(TransportConfig::default()),
    )
    .await
    .unwrap();
    let _ = stop.send(());
    actor.await.unwrap();
    driver.abort();
}
