//! Virtual network/receiver/device clocks around the production Engine and Renderer.
use super::*;
use fec::FecDecoder;
use mic_protocol::lab::{LabPacket, ParityEncoder, TransportConfig};
fn pcm(voice: bool) -> Vec<i16> {
    if !voice {
        return (0..16000)
            .map(|i| {
                (12000.0 * (i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin()).round()
                    as i16
            })
            .collect();
    }
    let b = include_bytes!("../tests/fixtures/1_jackson_0.wav");
    let mut pos = 12;
    let mut source = Vec::new();
    while pos + 8 <= b.len() {
        let n = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &b[pos..pos + 4] == b"data" {
            source = b[pos + 8..pos + 8 + n]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| i16::from_le_bytes([v[0], v[1]]))
                .collect();
            break;
        }
        pos += 8 + n + (n % 2);
    }
    assert!(!source.is_empty());
    let mut out = Vec::new();
    for i in 0..source.len() - 1 {
        out.push(source[i]);
        out.push(((source[i] as i32 + source[i + 1] as i32) / 2) as i16);
    }
    out
}
fn run(
    cfg: TransportConfig,
    enabled: bool,
    voice: bool,
    faults: bool,
    delay: u64,
) -> (u64, u64, u64, u64) {
    let mut e = Engine::new(3);
    e.playback.diagnostics = diagnostics::Diagnostics::bounded(128);
    e.jitter.diagnostics = Some(e.playback.diagnostics.clone());
    e.configure_transport(cfg, true);
    e.playback.diagnostics.set_enabled(enabled);
    e.playback.opened.store(true, Ordering::Relaxed);
    e.set_context("virtual-lab", "test-session");
    let source = pcm(voice);
    let count = cfg.samples();
    let period = cfg.frame_duration_ms as u64 * 1000;
    let key = b"public-test";
    let mut encoder = ParityEncoder::default();
    let mut decoder = FecDecoder::default();
    let mut packets = Vec::new();
    for seq in 0..(60_000_000 / period) as u32 {
        let mut samples = [0; 160];
        for (i, s) in samples[..count].iter_mut().enumerate() {
            *s = source[(seq as usize * count + i) % source.len()];
        }
        let p = LabPacket {
            audio: AudioPacket {
                sequence: seq,
                timestamp: seq.wrapping_mul(count as u32),
                session: [1; 16],
                samples,
            },
            parity: false,
            stream: 7,
            samples: count,
        };
        let parity = encoder.push(&p);
        let at = seq as u64 * period;
        if !faults || !matches!(seq, 100 | 200 | 201) {
            packets.push((
                at + if faults && seq % 113 == 0 {
                    delay * 1000
                } else {
                    0
                },
                p.encode(key),
            ));
        }
        if faults && seq == 101 {
            packets.push((at + 1000, p.encode(key)));
        }
        if cfg.fec.enabled
            && let Some(p) = parity
            && (!faults || p.audio.sequence != 304)
        {
            packets.push((at, p.encode(key)));
        }
    }
    packets.sort_by_key(|(t, _)| *t);
    let mut index = 0;
    let mut tick = 0;
    let mut callback = 0;
    let mut renderer = output::Renderer::with_options(16000, IsolationOptions::default());
    let mut checksum = 0u64;
    let mut previous = None;
    let mut out = [0f32; 160];
    while tick < 60_000_000 || callback < 60_000_000 {
        let arrival = packets.get(index).map_or(u64::MAX, |(t, _)| *t);
        let now = arrival.min(tick).min(callback);
        if now >= 60_000_000 {
            break;
        }
        if arrival == now {
            let p = LabPacket::decode(&packets[index].1, key, cfg, 7).unwrap();
            index += 1;
            if cfg.fec.enabled
                && let Some(r) =
                    decoder.receive(&p, now, e.jitter.expected_sequence(), e.jitter.started)
            {
                let original: Vec<_> = (0..count)
                    .map(|i| source[(r.sequence as usize * count + i) % source.len()])
                    .collect();
                assert_eq!(&r.samples[..count], original.as_slice());
                e.jitter.push(r);
            }
            if !p.parity {
                e.receive_at(p.audio, now);
            }
            while decoder.take_notice().is_some() {}
        } else if tick == now {
            let before = e.jitter.expected_sequence();
            let was = e.jitter.started;
            e.tick();
            if was && let Some(seq) = before {
                if let Some(prev) = previous {
                    assert_eq!(seq, prev + 1);
                }
                previous = Some(seq);
            }
            if (was || e.jitter.started)
                && let Some(seq) = before
            {
                decoder.advance_deadline(Some(seq.wrapping_add(1)));
            }
            decoder.expire(now);
            tick += period;
            assert!(e.jitter.occupancy() <= e.jitter.capacity);
            assert!(e.playback.queue.len() <= 1600);
        } else {
            // One bounded simulated device stall; callback state is the real Renderer.
            renderer.render(
                &mut out,
                1,
                &e.playback.queue,
                &e.playback.active,
                &e.playback.epoch,
            );
            for v in out {
                checksum = checksum.wrapping_mul(31).wrapping_add(v.to_bits() as u64);
            }
            callback += if faults && now == 3_000_000 {
                50_000
            } else {
                10_000
            };
        }
    }
    (
        checksum,
        e.jitter.metrics.lost_packets,
        decoder.metrics.recovered,
        decoder.metrics.too_late,
    )
}
#[test]
fn sixty_virtual_seconds_lab_matrix_sine_speech_delays_fec_and_diagnostics_checksum() {
    for (ms, fec, target) in [
        (10, false, 30),
        (5, false, 30),
        (5, true, 30),
        (5, true, 40),
        (5, true, 60),
    ] {
        let mut cfg = TransportConfig {
            frame_duration_ms: ms,
            jitter_buffer_target_ms: target,
            ..TransportConfig::default()
        };
        cfg.fec.enabled = fec;
        for voice in [false, true] {
            let healthy = run(cfg, true, voice, false, 0);
            assert_eq!(healthy, run(cfg, false, voice, false, 0));
            assert_eq!(healthy.1, 0);
            for delay in [2, 5, 10, 20] {
                let a = run(cfg, true, voice, true, delay);
                assert_eq!(a, run(cfg, false, voice, true, delay));
                if fec {
                    assert!(a.2 > 0);
                }
            }
        }
    }
}
