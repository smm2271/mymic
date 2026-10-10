//! Actual audio implementations under independent virtual clocks; no compensation.
use crate::{
    Declicker, IsolationOptions, Jitter,
    diagnostics::Diagnostics,
    output::{Input, Renderer},
};
use crossbeam_queue::ArrayQueue;
use mic_protocol::AudioPacket;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[derive(Default, Debug, PartialEq)]
pub(super) struct Result {
    pub(super) checksum: u64,
    pub(super) missing: u64,
    pub(super) capacity_drops: u64,
    starved: u64,
    clear: u64,
    max_jitter: usize,
    max_ring: usize,
    delivered: u64,
    pub(super) levels: Vec<(u64, usize, usize, u64, u64)>,
}
fn run(tx_ppm: i32, device_ppm: i32, enabled: bool, faults: bool, seconds: u64) -> Result {
    run_profile(tx_ppm, device_ppm, enabled, faults, seconds, None)
}
pub(super) fn run_profile(
    tx_ppm: i32,
    device_ppm: i32,
    enabled: bool,
    faults: bool,
    seconds: u64,
    mut investigation: Option<&mut super::clock_amplification_tests::Investigation>,
) -> Result {
    let diagnostics = Diagnostics::bounded(128);
    diagnostics.set_enabled(enabled);
    let mut jitter = Jitter::new(3);
    jitter.diagnostics = Some(diagnostics.clone());
    let mut declick = Declicker::new();
    let mut renderer = Renderer::with_options(16000, IsolationOptions::default());
    let queue = ArrayQueue::new(1600);
    let active = AtomicBool::new(false);
    let epoch = AtomicU64::new(0);
    let mut result = Result::default();
    let mut sequence = 0u32;
    let period = |ppm: i32| 10_000_000.0 / (1.0 + ppm as f64 / 1_000_000.0);
    let mut tx: f64 = 0.0;
    let mut tick = 10_000_000.0;
    let mut callback = 0.0;
    let mut delayed: Vec<(f64, AudioPacket)> = Vec::new();
    let mut pcm = [0f32; 160];
    let end = seconds as f64 * 1e9;
    let mut next_level = 1u64;
    let mut switched = None;
    loop {
        let arrival = delayed
            .iter()
            .map(|(t, _)| *t)
            .fold(f64::INFINITY, f64::min);
        let now = tx.min(tick).min(callback).min(arrival);
        if now >= end {
            break;
        }
        let before = investigation
            .as_ref()
            .map(|_| super::clock_amplification_tests::State::read(&jitter, &diagnostics));
        let mut packet_context = None;
        let kind;
        let prefer_tick = investigation
            .as_ref()
            .is_some_and(|o| o.tie_tick_first && o.first_drop_ns.is_some())
            && now == tick;
        if now == tx && !prefer_tick {
            kind = "generate";
            let p = AudioPacket {
                sequence,
                timestamp: sequence.wrapping_mul(160),
                session: [0; 16],
                samples: [(sequence % 300) as i16; 160],
            };
            packet_context = Some((p.sequence, p.timestamp));
            if !faults || !sequence.is_multiple_of(127) {
                delayed.push((
                    now + if faults && sequence.is_multiple_of(211) {
                        80_000_000.0
                    } else {
                        0.0
                    },
                    p,
                ));
            }
            sequence = sequence.wrapping_add(1);
            tx = if let Some((base_seq, base_time, ppm)) = switched {
                base_time + sequence.wrapping_sub(base_seq) as f64 * period(ppm)
            } else {
                sequence as f64 * period(tx_ppm)
            };
        } else if now == arrival && !prefer_tick {
            kind = "arrival";
            let i = delayed.iter().position(|(t, _)| *t == arrival).unwrap();
            let (_, p) = delayed.swap_remove(i);
            packet_context = Some((p.sequence, p.timestamp));
            jitter.push(p);
        } else if now == tick {
            kind = "playout";
            let was = jitter.started;
            let frame = jitter.tick();
            let missing = jitter.last_tick_missing;
            let upstream = missing || declick.in_underrun;
            let frame = declick.process(frame, missing);
            if jitter.started || was {
                if queue.len() > 960 {
                    while queue.pop().is_some() {}
                    epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    result.clear += 1;
                    active.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                let tag = epoch.load(std::sync::atomic::Ordering::Relaxed);
                for s in frame {
                    let _ = queue.push(Input {
                        value: s as f32 / 32768.0,
                        upstream_faded: upstream,
                        epoch: tag,
                    });
                }
                active.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            tick += 10_000_000.0;
        } else {
            kind = "callback";
            let stats = renderer.render(&mut pcm, 1, &queue, &active, &epoch);
            result.starved += stats.short as u64;
            for s in pcm {
                result.checksum = result
                    .checksum
                    .wrapping_mul(31)
                    .wrapping_add(s.to_bits() as u64);
            }
            callback += period(device_ppm);
        }
        if let Some(observer) = investigation.as_mut() {
            observer.observe(
                now,
                kind,
                packet_context,
                before.unwrap(),
                super::clock_amplification_tests::State::read(&jitter, &diagnostics),
                &diagnostics,
            );
            if switched.is_none()
                && observer.first_drop_ns.is_some()
                && let Some(ppm) = observer.switch_to
            {
                // Preserve the fraction remaining to the NEXT generation, not a
                // sequence*new_period time jump. Source indices never reset.
                tx = now + (tx - now) * period(ppm) / period(tx_ppm);
                switched = Some((sequence, tx, ppm));
                observer.switch_ns = Some(now);
            }
        }
        result.max_jitter = result.max_jitter.max(jitter.occupancy());
        result.max_ring = result.max_ring.max(queue.len());
        if diagnostics.queue.len() > 100 {
            diagnostics.drain();
        }
        diagnostics.discard_observations();
        assert!(delayed.len() < 16);
        assert!(jitter.occupancy() <= 8);
        assert!(queue.len() <= 1600);
        if now >= next_level as f64 * 1e9 {
            result.levels.push((
                next_level,
                jitter.occupancy(),
                queue.len(),
                jitter.metrics.lost_packets,
                jitter.metrics.jitter_buffer_drops,
            ));
            next_level += 1;
            assert!(result.levels.len() <= seconds as usize);
        }
    }
    result.missing = jitter.metrics.lost_packets;
    result.capacity_drops = jitter.metrics.jitter_buffer_drops;
    result.delivered = jitter.metrics.delivered_packets;
    result
}
#[test]
#[ignore = "long baseline matrix: run explicitly with --ignored; no compensation"]
fn thirty_virtual_minutes_independent_clock_matrix_and_observer_equivalence() {
    let mut rows = Vec::new();
    for ppm in [-1000, -500, -100, -50, 0, 50, 100, 500, 1000] {
        for (tx, device) in [(ppm, 0), (0, ppm)] {
            let on = run(tx, device, true, false, 1800);
            let off = run(tx, device, false, false, 1800);
            assert_eq!(
                on, off,
                "observer changed PCM / semantics tx={tx} device={device}"
            );
            if ppm == 0 {
                assert_eq!(on.capacity_drops, 0);
                assert_eq!(on.starved, 0);
            }
            rows.push(serde_json::json!({"tx_ppm":tx,"device_ppm":device,"virtual_seconds":1800,"baseline_uncompensated":true,"missing_slots":on.missing,"capacity_drops":on.capacity_drops,"callback_starvations":on.starved,"ring_clears":on.clear,"max_jitter":on.max_jitter,"max_ring":on.max_ring,"delivered":on.delivered,"on_off_equal":true}));
        }
    }
    if let Some(dir) = std::env::var_os("MIC_DRIFT_ARTIFACT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::PathBuf::from(dir).join("clock-matrix.json"),
            serde_json::to_vec_pretty(&rows).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn loss_delay_reorder_bursts_preserve_observed_baseline() {
    assert_eq!(run(100, 0, true, true, 60), run(100, 0, false, true, 60));
}

#[test]
#[ignore = "focused 30-minute baseline trace; run explicitly"]
fn export_positive_50ppm_occupancy_trace() {
    let on = run(50, 0, true, false, 1800);
    let off = run(50, 0, false, false, 1800);
    assert_eq!(on, off);
    assert!(on.capacity_drops > 0 && on.missing > 0);
    if let Some(dir) = std::env::var_os("MIC_DRIFT_ARTIFACT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let value = serde_json::json!({"tx_ppm":50,"device_ppm":0,"virtual_seconds":1800,"baseline_uncompensated":true,"population":"one per virtual second after an event; source samples in ring, packet count in jitter","columns":["second","jitter_packets","ring_samples","missing_slots_total","capacity_drops_total"],"levels":on.levels});
        std::fs::write(
            std::path::PathBuf::from(dir).join("positive-50ppm-occupancy.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn empty_reanchor_replays_concealed_slots_is_baseline_not_fixed() {
    let mut j = Jitter::new(2);
    let packet = |n| AudioPacket {
        sequence: n,
        timestamp: n * 160,
        session: [0; 16],
        samples: [123; 160],
    };
    j.push(packet(0));
    j.push(packet(1));
    j.tick();
    j.tick();
    for _ in 0..3 {
        j.tick();
    }
    assert_eq!(j.expected_sequence(), None);
    j.push(packet(2));
    j.push(packet(3));
    assert_eq!(j.tick()[0], 123);
    assert_eq!(j.expected_sequence(), Some(3));
}
