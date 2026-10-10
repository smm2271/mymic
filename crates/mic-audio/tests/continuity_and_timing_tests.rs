use mic_audio::{Declicker, Engine};
use mic_protocol::AudioPacket;

fn make_packet(seq: u32, samples: [i16; 160]) -> AudioPacket {
    AudioPacket {
        sequence: seq,
        timestamp: seq * 160,
        session: [1; 16],
        samples,
    }
}

fn sine_frame(phase: &mut f64, freq_hz: f64, amp: f64) -> [i16; 160] {
    let mut frame = [0i16; 160];
    for sample in &mut frame {
        *sample = (amp * phase.sin()).round() as i16;
        *phase += 2.0 * std::f64::consts::PI * freq_hz / 16000.0;
        if *phase > 2.0 * std::f64::consts::PI {
            *phase -= 2.0 * std::f64::consts::PI;
        }
    }
    frame
}

#[test]
fn test_audio_continuity_fade_transitions_and_boundary_checks() {
    let mut declicker = Declicker::new();

    // 1. Continuous normal PCM remains completely unmodified
    let continuous = [1000i16; 160];
    assert_eq!(declicker.process(continuous, false), continuous);
    assert_eq!(declicker.fade_outs, 0);
    assert_eq!(declicker.fade_ins, 0);

    // 2. Interruption at non-zero amplitude triggers smooth fade-out
    let missing = [0i16; 160];
    let fade_out_frame = declicker.process(missing, true);
    assert_eq!(declicker.fade_outs, 1);
    assert!(fade_out_frame[0] > 0);
    assert!(fade_out_frame[0] < 1000);
    assert_eq!(fade_out_frame[48], 0);
    assert_eq!(fade_out_frame[159], 0);

    // 3. Sustained underrun outputs pure silence without repeating old audio
    let silent_frame = declicker.process(missing, true);
    assert_eq!(silent_frame, [0i16; 160]);
    assert_eq!(declicker.fade_outs, 1); // Does not re-trigger fade-out on already silent stream

    // 4. Recovery fade-in smoothly ascends from silence
    let resume = [2000i16; 160];
    let fade_in_frame = declicker.process(resume, false);
    assert_eq!(declicker.fade_ins, 1);
    assert!(fade_in_frame[0] <= 5);
    assert!(fade_in_frame[24] > 0 && fade_in_frame[24] < 2000);
    assert_eq!(fade_in_frame[48], 2000);
    assert_eq!(fade_in_frame[159], 2000);

    // 5. High amplitude edge case without i16 overflow
    let max_amp = [i16::MAX; 160];
    let max_out = declicker.process(max_amp, false);
    assert_eq!(max_out, max_amp);
}

#[test]
fn test_sine_wave_interruption_at_peak_amplitude() {
    let mut declicker = Declicker::new();
    let mut phase = 0.0;
    // Advance sine wave to near positive peak (~+20,000)
    let frame1 = sine_frame(&mut phase, 440.0, 20000.0);
    let out1 = declicker.process(frame1, false);
    assert_eq!(out1, frame1);

    // Interrupted: frame is missing
    let missing = [0i16; 160];
    let out_fade = declicker.process(missing, true);
    assert!(out_fade[0] > 0);
    // Smoothly decays to zero
    assert_eq!(out_fade[159], 0);
    assert_eq!(declicker.fade_outs, 1);
}

#[test]
fn test_engine_packet_timing_discontinuities_and_reordering() {
    let mut engine = Engine::new(2);

    // Feed packets 0, 1, 2
    for seq in 0..3 {
        engine.receive(make_packet(seq, [100; 160]));
        engine.tick();
    }

    // Out-of-order & Late packets
    // Packet 5 arrives before packet 4
    engine.receive(make_packet(5, [100; 160]));
    engine.tick();
    // Packet 4 arrives out-of-order
    engine.receive(make_packet(4, [100; 160]));
    engine.tick();

    // Duplicate packet 4
    engine.receive(make_packet(4, [100; 160]));
    engine.tick();

    let m = engine.metrics();
    assert_eq!(m.duplicate_packets, 1);
    assert!(m.jitter_buffer_depth <= 4);

    // Resetting engine clears stream state without corrupting metrics
    engine.reset();
    assert_eq!(engine.metrics().jitter_buffer_depth, 0);
    assert_eq!(engine.metrics().ring_depth_samples, 0);

    engine.playback.shutdown();
}
