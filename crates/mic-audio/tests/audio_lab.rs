use mic_audio::{Engine, fec::FecDecoder};
use mic_protocol::{
    AudioPacket,
    lab::{LabPacket, ParityEncoder, TransportConfig},
};
fn group(base: u32, ms: u16) -> (Vec<LabPacket>, LabPacket) {
    let mut enc = ParityEncoder::default();
    let cfg = TransportConfig {
        frame_duration_ms: ms,
        ..TransportConfig::default()
    };
    let mut packets = Vec::new();
    let mut parity = None;
    for i in 0..4 {
        let n = base.wrapping_add(i);
        let mut pcm = [0; 160];
        for (j, v) in pcm[..cfg.samples()].iter_mut().enumerate() {
            *v = (n as i16).wrapping_mul(333).wrapping_add(j as i16);
        }
        let p = LabPacket {
            audio: AudioPacket {
                sequence: n,
                timestamp: n.wrapping_mul(cfg.samples() as u32),
                session: [1; 16],
                samples: pcm,
            },
            parity: false,
            stream: 1,
            samples: cfg.samples(),
        };
        parity = enc.push(&p).or(parity);
        packets.push(p);
    }
    (packets, parity.unwrap())
}
#[test]
fn xor_each_missing_wrap_reorder_duplicate_and_original_pcm() {
    for ms in [5, 10] {
        for base in [0, u32::MAX - 3] {
            for missing in 0..4 {
                let (data, parity) = group(base, ms);
                let mut f = FecDecoder::default();
                let mut out = None;
                for p in std::iter::once(&parity).chain(
                    data.iter()
                        .rev()
                        .filter(|p| p.audio.sequence != data[missing].audio.sequence),
                ) {
                    out = f.receive(p, 10, None, false).or(out);
                }
                let r = out.unwrap();
                assert_eq!(r.samples, data[missing].audio.samples);
                assert_eq!(r.timestamp, data[missing].audio.timestamp);
                assert_eq!(r.sequence, data[missing].audio.sequence);
                assert_eq!(f.metrics.recovered, 1);
                for p in &data {
                    assert!(f.receive(p, 20, None, false).is_none());
                }
                assert!(f.receive(&parity, 20, None, false).is_none());
                assert_eq!(f.metrics.recovered, 1);
            }
        }
    }
}
#[test]
fn two_losses_no_parity_expiry_capacity_session_and_deadline() {
    let (data, parity) = group(0, 5);
    let mut f = FecDecoder::default();
    for p in [&data[0], &data[3], &parity] {
        assert!(f.receive(p, 0, None, false).is_none());
    }
    f.expire(FecDecoder::TTL_US);
    assert_eq!(f.metrics.unrecoverable, 1);
    assert_eq!(f.occupancy(), 0);
    f.clear();
    for p in [&data[0], &data[1], &data[3]] {
        assert!(f.receive(p, 0, None, false).is_none());
    }
    let mut wrong = parity.clone();
    wrong.audio.session = [2; 16];
    assert!(f.receive(&wrong, 0, None, false).is_none());
    f.advance_deadline(Some(4));
    assert!(f.receive(&parity, 1, None, false).is_none());
    assert_eq!(f.metrics.too_late, 1); // watermark survives empty rebuffer
    f.clear();
    for i in 0..100 {
        let (d, _) = group(i * 4, 5);
        f.receive(&d[0], 0, None, false);
        assert!(f.occupancy() <= 32);
    }
    assert!(f.metrics.capacity_evictions > 0);
    f.clear();
    for p in &data {
        assert!(f.receive(p, 0, None, false).is_none());
    }
    assert!(f.receive(&parity, 0, None, false).is_none());
}
#[test]
fn actual_jitter_config_deadline_recovery_switch_and_sample_count() {
    for ms in [5, 10] {
        for target in [20, 30, 40, 50, 60, 80, 100] {
            let cfg = TransportConfig {
                frame_duration_ms: ms,
                jitter_buffer_target_ms: target,
                ..TransportConfig::default()
            };
            let mut e = Engine::new(3);
            e.configure_transport(cfg, true);
            assert_eq!(e.jitter.depth, cfg.depth());
            assert!(e.jitter.capacity > e.jitter.depth);
            for n in 0..cfg.depth() as u32 {
                let (data, _) = group(n & !3, ms);
                e.receive(data[(n % 4) as usize].audio.clone());
            }
            for _ in 0..cfg.depth() {
                e.tick();
            }
            assert_eq!(
                e.metrics().delivered_samples,
                (cfg.depth() * cfg.samples()) as u64
            );
            for _ in 0..5 {
                e.tick();
            }
            assert!(!e.jitter.started);
            let (data, _) = group(100, ms);
            for p in data {
                e.receive(p.audio);
            } // safe explicit config/start epoch below
            e.configure_transport(TransportConfig::default(), false);
            assert_eq!(e.jitter.occupancy(), 0);
            assert_eq!(e.jitter.capacity, 8);
            assert_eq!(e.jitter.frame_samples, 160);
        }
    }
}
#[test]
fn final_empty_deadline_is_also_protected_after_rebuffer() {
    let cfg = TransportConfig {
        frame_duration_ms: 5,
        jitter_buffer_target_ms: 20,
        ..TransportConfig::default()
    };
    let mut e = Engine::new(3);
    e.configure_transport(cfg, true);
    let (data, _) = group(4, 5);
    let mut f = FecDecoder::default();
    // Four delivered slots followed by three concealed slots.
    for p in [&data[0], &data[1], &data[2], &data[3]] {
        e.receive(p.audio.clone());
    }
    for _ in 0..7 {
        let before = e.jitter.expected_sequence();
        let was = e.jitter.started;
        e.tick();
        if (was || e.jitter.started)
            && let Some(seq) = before
        {
            f.advance_deadline(Some(seq.wrapping_add(1)));
        }
    }
    assert!(!e.jitter.started);
    assert_eq!(e.jitter.expected_sequence(), None);
    let (later, _) = group(8, 5);
    for p in [&later[0], &later[1], &later[3]] {
        assert!(f.receive(p, 1, None, false).is_none());
    }
    // Missing seq10 was the final consumed empty slot; a matching group parity is too late.
    let mut g = ParityEncoder::default();
    let mut lp = None;
    for p in &later {
        lp = g.push(p).or(lp);
    }
    assert!(f.receive(&lp.unwrap(), 2, None, false).is_none());
    assert_eq!(f.metrics.too_late, 1);
}
