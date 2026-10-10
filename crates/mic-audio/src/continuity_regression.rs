use super::*;
use output::{Input, Renderer};
fn packet(n: u32) -> AudioPacket {
    AudioPacket {
        sequence: n,
        timestamp: n.wrapping_mul(160),
        session: [0; 16],
        samples: std::array::from_fn(|i| {
            (10000.0
                * (std::f64::consts::TAU * 440.0 * (n as f64 * 160.0 + i as f64) / 16000.0).sin())
            .round() as i16
        }),
    }
}
fn put(queue: &ArrayQueue<Input>, samples: impl IntoIterator<Item = f32>, faded: bool, epoch: u64) {
    for value in samples {
        assert!(
            queue
                .push(Input {
                    value,
                    upstream_faded: faded,
                    epoch
                })
                .is_ok()
        );
    }
}
fn max_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|s| (s[1] - s[0]).abs())
        .fold(0.0, f32::max)
}
#[test]
fn capacity_keeps_contiguous_audio_phase0_path() {
    let mut j = Jitter::new(3);
    let mut d = Declicker::new();
    for n in 0..3 {
        j.push(packet(n));
    }
    let previous = d.process(j.tick(), j.last_tick_missing);
    for n in 3..10 {
        j.push(packet(n));
    }
    assert_eq!(j.expected_sequence(), Some(1));
    assert_eq!(j.occupancy(), 8);
    assert_eq!(j.contiguous_playable_depth(), 8);
    assert_eq!(j.metrics.jitter_buffer_drops, 1);
    let next = d.process(j.tick(), j.last_tick_missing);
    assert_eq!(next, packet(1).samples);
    assert_eq!(d.fade_ins, 0);
    // Distinguish the expected derivative at this exact phase from a skipped-frame seam.
    let correct = packet(1).samples[0] as i32 - packet(0).samples[159] as i32;
    assert_eq!(next[0] as i32 - previous[159] as i32, correct);
    let old_jump = packet(9).samples[0] as i32 - previous[159] as i32;
    assert!((old_jump - correct).abs() > 10000);
}
#[test]
fn full_buffer_accepts_missing_expected_by_replacing_only_farthest() {
    let mut j = Jitter::new(3);
    for n in 0..3 {
        j.push(packet(n));
    }
    for _ in 0..3 {
        j.tick();
    }
    for n in 4..12 {
        j.push(packet(n));
    }
    assert_eq!(j.expected_sequence(), Some(3));
    assert_eq!(j.contiguous_playable_depth(), 0);
    j.push(packet(3));
    assert_eq!(j.occupancy(), 8);
    assert_eq!(j.contiguous_playable_depth(), 8);
    assert!(!j.packets.contains_key(&11));
    assert_eq!(j.tick(), packet(3).samples);
    for n in 4..11 {
        assert_eq!(j.tick(), packet(n).samples);
    }
    assert_eq!(j.metrics.lost_packets, 0);
}
#[test]
fn wrap_reorder_duplicates_late_and_far_admission() {
    let mut j = Jitter::new(3);
    let first = u32::MAX - 3;
    for i in 0..8 {
        j.push(packet(first.wrapping_add(i)));
    }
    j.push(packet(first.wrapping_add(8))); // farther new packet rejected, no cursor jump
    assert_eq!(j.expected_sequence(), Some(first));
    assert_eq!(j.contiguous_playable_depth(), 8);
    for i in 0..8 {
        assert_eq!(j.tick(), packet(first.wrapping_add(i)).samples);
    }
    j.push(packet(first));
    assert_eq!(j.metrics.duplicate_packets, 1);
    let cursor = j.expected_sequence();
    j.push(packet(cursor.unwrap().wrapping_add(101)));
    j.push(packet(cursor.unwrap().wrapping_add(0x8000_0000)));
    assert_eq!(j.expected_sequence(), cursor);
    assert_eq!(j.occupancy(), 0);
    let mut j = Jitter::new(3);
    for n in [2, 0, 1] {
        j.push(packet(n));
    }
    for n in 0..3 {
        assert_eq!(j.tick(), packet(n).samples);
    }
    // Old unseen packet is late, not a duplicate. (Sequence wraps backward.)
    j.push(packet(u32::MAX));
    assert_eq!(j.metrics.late_packet_drops, 1);
}
#[test]
fn burst_retains_eight_nearest_and_has_observable_capacity_reasons() {
    let d = diagnostics::Diagnostics::bounded(64);
    let mut j = Jitter::new(3);
    j.diagnostics = Some(d.clone());
    for n in 1849..1864 {
        j.push(packet(n));
    }
    assert_eq!(j.expected_sequence(), Some(1849));
    assert_eq!(j.occupancy(), 8);
    for n in 1849..1857 {
        assert_eq!(j.tick(), packet(n).samples);
    }
    assert_eq!(j.metrics.lost_packets, 0);
    assert!(
        d.drain()
            .iter()
            .all(|r| r.event.reason == "jitter_capacity_reject_farther")
    );
    assert_eq!(j.metrics.jitter_buffer_drops, 7);
}
#[test]
fn callback_starvation_recovery_smooths_final_samples() {
    for rate in [16000, 44100, 48000] {
        let queue = ArrayQueue::new(1600);
        let active = AtomicBool::new(true);
        let epoch = AtomicU64::new(0);
        let mut r = Renderer::new(rate);
        let length = (rate / 100) as usize;
        put(&queue, [0.3; 320], false, 0);
        let mut first = vec![0.0; length];
        r.render(&mut first, 1, &queue, &active, &epoch);
        let mut short = vec![0.0; length];
        assert!(r.render(&mut short, 1, &queue, &active, &epoch).short);
        let mut silence = vec![0.0; length];
        r.render(&mut silence, 1, &queue, &active, &epoch);
        assert_eq!(*silence.last().unwrap(), 0.0);
        put(&queue, [-0.3; 320], false, 0);
        let mut resumed = vec![0.0; length];
        assert!(!r.render(&mut resumed, 1, &queue, &active, &epoch).short);
        assert_eq!(resumed[0], 0.0);
        assert_eq!(*resumed.last().unwrap(), -0.3);
        let mut all = first;
        all.extend(short);
        all.extend(silence);
        all.extend(resumed);
        // Analytic smoothstep slope <=1.5*amplitude/(fade_frames-1), with rounding slack.
        assert!(max_step(&all) <= 1.6 * 0.3 / (rate as f32 * 0.003 - 1.0));
    }
}
#[test]
fn epoch_reset_cannot_reuse_old_interpolation_history() {
    let queue = ArrayQueue::new(1600);
    let active = AtomicBool::new(true);
    let epoch = AtomicU64::new(0);
    let mut r = Renderer::new(48000);
    put(&queue, [0.3; 640], false, 0);
    let mut old = [0.0; 480];
    r.render(&mut old, 1, &queue, &active, &epoch);
    // Keep old tagged samples in queue to exercise consumer-side epoch filtering.
    epoch.store(1, Ordering::Release);
    put(&queue, [-0.3; 640], false, 1);
    let mut new = [0.0; 480];
    r.render(&mut new, 1, &queue, &active, &epoch);
    assert_eq!(new[0], old[479]);
    assert_eq!(new[143], -0.3);
    assert_eq!(new[479], -0.3);
    assert!(max_step(&new) <= 1.6 * 0.6 / 143.0);
}
#[test]
fn upstream_declick_is_not_multiplied_by_another_gain_fade() {
    let queue = ArrayQueue::new(1600);
    let active = AtomicBool::new(false);
    let epoch = AtomicU64::new(0);
    let mut r = Renderer::new(16000);
    let mut quiet = [0.0; 160];
    r.render(&mut quiet, 1, &queue, &active, &epoch);
    let mut d = Declicker::new();
    d.process([10000; 160], false);
    d.process([0; 160], true);
    let faded = d.process([10000; 160], false);
    put(&queue, faded.map(|v| v as f32 / 32768.0), true, 0);
    put(&queue, [10000.0 / 32768.0; 160], false, 0);
    active.store(true, Ordering::Release);
    let mut out = [0.0; 160];
    r.render(&mut out, 1, &queue, &active, &epoch);
    for i in 0..160 {
        assert!((out[i] - faded[i] as f32 / 32768.0).abs() < 0.0004);
    }
    assert_eq!(out[48], faded[48] as f32 / 32768.0);
}
#[test]
fn healthy_440hz_is_transparent_and_has_no_added_fixed_delay() {
    let queue = ArrayQueue::new(1600);
    let active = AtomicBool::new(true);
    let epoch = AtomicU64::new(0);
    let samples: Vec<f32> = (0..640)
        .map(|n| 0.3 * (std::f32::consts::TAU * 440.0 * n as f32 / 16000.0).sin())
        .collect();
    put(&queue, samples.iter().copied(), false, 0);
    let mut renderer = Renderer::new(48000);
    let mut actual = [0.0; 960];
    renderer.render(&mut actual, 1, &queue, &active, &epoch);
    let mut legacy = Resampler::new(48000);
    let mut input = samples.into_iter();
    for sample in actual {
        assert_eq!(sample, legacy.next(|| input.next()).0);
    }
}
fn speech() -> Vec<f32> {
    let bytes = include_bytes!("../tests/fixtures/1_jackson_0.wav");
    assert_eq!(&bytes[0..4], b"RIFF");
    let mut position = 12;
    let mut pcm = Vec::new();
    while position + 8 <= bytes.len() {
        let length =
            u32::from_le_bytes(bytes[position + 4..position + 8].try_into().unwrap()) as usize;
        let chunk = &bytes[position + 8..position + 8 + length];
        if &bytes[position..position + 4] == b"fmt " {
            assert_eq!(u16::from_le_bytes(chunk[0..2].try_into().unwrap()), 1);
            assert_eq!(u16::from_le_bytes(chunk[2..4].try_into().unwrap()), 1);
            assert_eq!(u32::from_le_bytes(chunk[4..8].try_into().unwrap()), 8000);
        }
        if &bytes[position..position + 4] == b"data" {
            pcm = chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
                .collect();
        }
        position += 8 + length + (length % 2);
    }
    assert!(pcm.len() > 4000);
    // Explicit 8k -> 16k interpolation, not a mislabeled WAV.
    pcm.windows(2)
        .flat_map(|p| [p[0], (p[0] + p[1]) * 0.5])
        .collect()
}
#[test]
fn prerecorded_speech_preserves_healthy_pcm_and_bounds_reset_residual() {
    let source = speech();
    let queue = ArrayQueue::new(source.len() + 640);
    let active = AtomicBool::new(true);
    let epoch = AtomicU64::new(0);
    put(&queue, source.iter().copied(), false, 0);
    let mut renderer = Renderer::new(16000);
    let peak = source
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    let cut = peak.clamp(160, source.len() - 640);
    let mut first = vec![0.0; cut];
    renderer.render(&mut first, 1, &queue, &active, &epoch);
    assert_eq!(first, source[..cut]);
    while queue.pop().is_some() {}
    epoch.store(1, Ordering::Release);
    let start = (cut + 160).min(source.len() - 321);
    put(&queue, source[start..].iter().copied(), false, 1);
    let mut next = [0.0; 160];
    renderer.render(&mut next, 1, &queue, &active, &epoch);
    assert_eq!(next[0], first[cut - 1]);
    assert_eq!(&next[48..], &source[start + 48..start + 160]);
    // Remove ordinary speech derivatives and examine just the transition residual.
    let residual: Vec<f32> = next
        .iter()
        .zip(&source[start..])
        .map(|(a, b)| a - b)
        .collect();
    let mut residual_step = 0.0f32;
    for i in 1..48 {
        let w = (i as f32 / 47.0).powi(2) * (3.0 - 2.0 * i as f32 / 47.0);
        assert!((next[i] - (source[start + i] * w + first[cut - 1] * (1.0 - w))).abs() < 1e-6);
        residual_step = residual_step.max((residual[i] - residual[i - 1]).abs());
    }
    let source_step = max_step(&source[start..start + 48]);
    let peak = source[start..start + 48]
        .iter()
        .map(|s| s.abs())
        .fold(0.0, f32::max);
    assert!(residual_step <= source_step + 1.6 * (first[cut - 1].abs() + peak) / 47.0);
}

#[test]
fn recovery_440hz_bound_accounts_for_frequency_phase_and_envelope() {
    let rate = 48000;
    let amplitude = 0.3;
    let natural_step = 2.0 * amplitude * (std::f32::consts::PI * 440.0 / rate as f32).sin();
    let envelope_step = 1.6 * amplitude / 143.0;
    for phase in (0..16).map(|n| n as f32 * std::f32::consts::TAU / 16.0) {
        let queue = ArrayQueue::new(1600);
        let active = AtomicBool::new(false);
        let epoch = AtomicU64::new(0);
        let mut r = Renderer::new(rate);
        let mut silence = [0.0; 480];
        r.render(&mut silence, 1, &queue, &active, &epoch);
        let source: Vec<f32> = (0..320)
            .map(|n| amplitude * (phase + std::f32::consts::TAU * 440.0 * n as f32 / 16000.0).sin())
            .collect();
        put(&queue, source.iter().copied(), false, 0);
        active.store(true, Ordering::Release);
        let mut resumed = [0.0; 480];
        r.render(&mut resumed, 1, &queue, &active, &epoch);
        assert_eq!(resumed[0], 0.0);
        assert!(max_step(&resumed) <= natural_step + envelope_step + 1e-5);
        // A healthy phase-zero crossing is not classified as a pop solely due to amplitude.
        let mut baseline = Resampler::new(rate);
        let mut input = source.into_iter();
        for (i, sample) in resumed.iter().enumerate() {
            let normal = baseline.next(|| input.next()).0;
            if i >= 144 {
                assert_eq!(*sample, normal);
            }
        }
    }
}
#[test]
fn output_conversion_and_channel_replication_use_smoothed_samples() {
    let queue = ArrayQueue::new(1600);
    let active = AtomicBool::new(true);
    let epoch = AtomicU64::new(0);
    put(&queue, [0.25; 640], false, 0);
    let mut r = Renderer::new(48000);
    let mut out = [0i16; 960];
    r.render(&mut out, 2, &queue, &active, &epoch);
    assert!(
        out.as_chunks::<2>()
            .0
            .iter()
            .all(|f| f[0] == f[1] && f[0] == 8192)
    );
    active.store(false, Ordering::Release);
    let mut fade = [0i16; 960];
    r.render(&mut fade, 2, &queue, &active, &epoch);
    assert_eq!(fade[0], 8192);
    assert_eq!(fade[958], 0);
    assert!(fade.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]));
    let mut r = Renderer::new(48000);
    let mut unsigned = [0u16; 960];
    r.render(&mut unsigned, 2, &queue, &active, &epoch);
    assert_eq!(unsigned, [32768; 960]);
}

// Optional evidence export is deliberately confined to a test, never the renderer.
#[test]
fn export_continuity_comparisons() {
    let Ok(directory) = std::env::var("MIC_CONTINUITY_ARTIFACT_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&directory);
    std::fs::create_dir_all(dir).unwrap();
    fn wav(path: &std::path::Path, samples: &[f32], rate: u32) {
        let size = samples.len() as u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + size).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(size.to_le_bytes());
        for s in samples {
            bytes.extend(((*s * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }
    let mut evidence = Vec::new();
    for name in ["callback-440", "reset-440"] {
        let queue = ArrayQueue::new(1600);
        let active = AtomicBool::new(true);
        let epoch = AtomicU64::new(0);
        let mut new = Renderer::new(48000);
        let mut old = Resampler::new(48000);
        let mut old_queue = std::collections::VecDeque::new();
        let tone = |phase: f32, n: usize| {
            (0..n)
                .map(move |i| {
                    0.3 * (phase + std::f32::consts::TAU * 440.0 * i as f32 / 16000.0).sin()
                })
                .collect::<Vec<_>>()
        };
        let initial = tone(0.7, if name == "callback-440" { 320 } else { 640 });
        put(&queue, initial.iter().copied(), false, 0);
        old_queue.extend(initial);
        let mut before = Vec::new();
        let mut after = Vec::new();
        let initial_batches = if name == "callback-440" { 3 } else { 1 };
        for _ in 0..initial_batches {
            let mut batch = [0.0; 480];
            new.render(&mut batch, 1, &queue, &active, &epoch);
            after.extend(batch);
            let ready = old.primed || old_queue.len() >= 320;
            let mut short = false;
            for _ in 0..480 {
                if ready {
                    let (s, missing) = old.next(|| old_queue.pop_front());
                    short |= missing;
                    before.push(s);
                } else {
                    before.push(0.0);
                }
            }
            if !ready || short {
                old.reset();
            }
        }
        if name == "reset-440" {
            while queue.pop().is_some() {}
            old_queue.clear();
            old.reset();
            epoch.store(1, Ordering::Release);
        }
        let recovered = tone(-std::f32::consts::FRAC_PI_2, 640);
        put(
            &queue,
            recovered.iter().copied(),
            false,
            epoch.load(Ordering::Acquire),
        );
        old_queue.extend(recovered);
        let seam = after.len();
        let mut batch = [0.0; 480];
        new.render(&mut batch, 1, &queue, &active, &epoch);
        after.extend(batch);
        for _ in 0..480 {
            before.push(old.next(|| old_queue.pop_front()).0);
        }
        wav(&dir.join(format!("{name}-before.wav")), &before, 48000);
        wav(&dir.join(format!("{name}-after.wav")), &after, 48000);
        let natural = 2.0 * 0.3 * (std::f32::consts::PI * 440.0 / 48000.0).sin();
        evidence.push(serde_json::json!({"case":name,"rate":48000,"seam":seam,"before_seam_delta":before[seam]-before[seam-1],"after_seam_delta":after[seam]-after[seam-1],"natural_440_step_bound":natural}));
    }
    let mut j = Jitter::new(3);
    for n in 0..3 {
        j.push(packet(n));
    }
    let first = j.tick();
    let mut records = String::from("action,sequence,expected,occupancy,contiguous,dropped\n");
    for n in 3..10 {
        j.push(packet(n));
        records.push_str(&format!(
            "push,{n},{},{},{},{}\n",
            j.expected_sequence().unwrap(),
            j.occupancy(),
            j.contiguous_playable_depth(),
            j.metrics.jitter_buffer_drops
        ));
    }
    let next = j.tick();
    let before: Vec<f32> = first
        .into_iter()
        .chain(packet(9).samples)
        .map(|s| s as f32 / 32768.0)
        .collect();
    let after: Vec<f32> = first
        .into_iter()
        .chain(next)
        .map(|s| s as f32 / 32768.0)
        .collect();
    wav(&dir.join("capacity-440-before.wav"), &before, 16000);
    wav(&dir.join("capacity-440-after.wav"), &after, 16000);
    evidence.push(serde_json::json!({"case":"capacity-440","rate":16000,"seam":160,"before_seam_delta":before[160]-before[159],"after_seam_delta":after[160]-after[159],"expected_natural_delta":packet(1).samples[0] as i32-packet(0).samples[159] as i32}));
    std::fs::write(dir.join("buffer-after.csv"), records).unwrap();
    std::fs::write(dir.join("buffer-before.csv"),"action,sequence,expected,occupancy,contiguous,dropped\npush,8,1,8,8,0\npush,9,9,1,1,8\npush,10,9,2,2,8\npush,11,9,3,3,8\n").unwrap();
    let source = speech();
    let cut = source
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0
        .clamp(160, source.len() - 640);
    let queue = ArrayQueue::new(source.len() + 640);
    let active = AtomicBool::new(true);
    let epoch = AtomicU64::new(0);
    let mut renderer = Renderer::new(16000);
    put(&queue, source.iter().copied(), false, 0);
    let mut after = vec![0.0; cut];
    renderer.render(&mut after, 1, &queue, &active, &epoch);
    while queue.pop().is_some() {}
    epoch.store(1, Ordering::Release);
    let start = cut + 160;
    put(&queue, source[start..].iter().copied(), false, 1);
    let mut resumed = [0.0; 160];
    renderer.render(&mut resumed, 1, &queue, &active, &epoch);
    after.extend(resumed);
    let before: Vec<f32> = source[..cut]
        .iter()
        .chain(&source[start..start + 160])
        .copied()
        .collect();
    wav(&dir.join("speech-reset-before.wav"), &before, 16000);
    wav(&dir.join("speech-reset-after.wav"), &after, 16000);
    evidence.push(serde_json::json!({"case":"speech-reset","rate":16000,"seam":cut,"before_seam_delta":before[cut]-before[cut-1],"after_seam_delta":after[cut]-after[cut-1],"fixture":"FSDD 1_jackson_0, explicitly upsampled 8k->16k"}));
    std::fs::write(
        dir.join("comparison.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
}
