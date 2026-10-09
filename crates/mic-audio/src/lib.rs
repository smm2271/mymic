mod sequence;
use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use mic_protocol::{AudioPacket, ahead};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
/// Continuous 16 kHz mono to device-rate linear interpolation, allocation-free.
struct Resampler {
    step: f64,
    phase: f64,
    a: f32,
    b: f32,
    primed: bool,
}
impl Resampler {
    fn new(rate: u32) -> Self {
        Self {
            step: 16000.0 / rate as f64,
            phase: 0.0,
            a: 0.0,
            b: 0.0,
            primed: false,
        }
    }
    fn reset(&mut self) {
        self.primed = false;
        self.phase = 0.0;
    }
    fn next(&mut self, mut input: impl FnMut() -> Option<f32>) -> (f32, bool) {
        let mut short = false;
        if !self.primed {
            self.a = input().unwrap_or(0.0);
            self.b = input().unwrap_or(0.0);
            self.primed = true;
        }
        let sample = self.a + (self.b - self.a) * self.phase as f32;
        self.phase += self.step;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.a = self.b;
            self.b = input().unwrap_or_else(|| {
                short = true;
                0.0
            });
        }
        (sample, short)
    }
}
#[derive(Default, Clone, Serialize, Debug)]
pub struct Metrics {
    pub received_packets: u64,
    pub lost_packets: u64,
    pub out_of_order_packets: u64,
    pub duplicate_packets: u64,
    pub receive_bitrate: u64,
    pub jitter_buffer_depth: usize,
    pub buffer_underruns: u64,
    pub buffer_overruns: u64,
    pub invalid_packets: u64,
    pub output_state: String,
    pub received_datagrams: u64,
    pub sequence_gaps: u64,
    pub sequence_out_of_order: u64,
    pub rejected_packets: u64,
    pub authentication_failures: u64,
    pub session_rejections: u64,
    pub invalid_packet_headers: u64,
    pub unpaired_datagrams: u64,
    pub accepted_packets: u64,
    pub delivered_packets: u64,
    pub delivered_samples: u64,
    pub late_packet_drops: u64,
    pub jitter_buffer_drops: u64,
    pub output_frames_requested: u64,
    pub output_samples_requested: u64,
    pub input_samples_consumed: u64,
    pub callback_starvations: u64,
    pub receiver_clock_resets: u64,
    pub ring_depth_samples: usize,
    pub output_sample_rate: u64,
    pub output_channels: u64,
}
pub struct Jitter {
    packets: HashMap<u32, AudioPacket>,
    expected: Option<u32>,
    started: bool,
    depth: usize,
    pub metrics: Metrics,
    seen: std::collections::VecDeque<u32>,
    empty_ticks: u8,
    muted: bool,
}
impl Jitter {
    pub fn new(depth: usize) -> Self {
        Self {
            packets: HashMap::new(),
            expected: None,
            started: false,
            depth: depth.clamp(2, 4),
            metrics: Metrics::default(),
            seen: std::collections::VecDeque::new(),
            empty_ticks: 0,
            muted: false,
        }
    }
    pub fn push(&mut self, p: AudioPacket) {
        self.metrics.received_packets += 1;
        if self.seen.contains(&p.sequence) {
            self.metrics.duplicate_packets += 1;
            return;
        }
        self.seen.push_back(p.sequence);
        if self.seen.len() > 64 {
            self.seen.pop_front();
        }
        if let Some(e) = self.expected {
            if ahead(e, p.sequence) {
                self.metrics.out_of_order_packets += 1;
                if !self.started && e.wrapping_sub(p.sequence) < 8 {
                    self.expected = Some(p.sequence)
                } else {
                    self.metrics.late_packet_drops += 1;
                    return;
                }
            } else if p.sequence != e.wrapping_add(self.packets.len() as u32) {
                self.metrics.out_of_order_packets += 1;
            }
        } else {
            self.expected = Some(p.sequence)
        }
        if self.packets.len() >= 8 {
            self.metrics.buffer_overruns += 1;
            self.metrics.jitter_buffer_drops += self.packets.len() as u64;
            self.packets.clear();
            self.expected = Some(p.sequence);
            self.started = false;
        }
        if self
            .expected
            .is_some_and(|e| p.sequence.wrapping_sub(e) > 100 && ahead(p.sequence, e))
        {
            self.packets.clear();
            self.expected = Some(p.sequence);
            self.started = false;
            self.metrics.buffer_overruns += 1;
        }
        self.metrics.accepted_packets += 1;
        self.packets.insert(p.sequence, p);
        self.metrics.jitter_buffer_depth = self.packets.len();
    }
    pub fn tick(&mut self) -> [i16; 160] {
        if !self.started {
            if self.packets.len() < self.depth {
                return [0; 160];
            };
            self.started = true;
        }
        let e = self.expected.unwrap();
        self.expected = Some(e.wrapping_add(1));
        let output = if let Some(p) = self.packets.remove(&e) {
            self.empty_ticks = 0;
            self.metrics.delivered_packets += 1;
            self.metrics.delivered_samples += 160;
            p.samples
        } else {
            self.metrics.lost_packets += 1;
            if !self.muted {
                self.metrics.buffer_underruns += 1;
            }
            self.empty_ticks += 1;
            if self.empty_ticks >= 3 && self.packets.is_empty() {
                // A stalled sender/scheduler must not leave the playout cursor ahead forever.
                self.started = false;
                self.expected = None;
                self.empty_ticks = 0;
            }
            [0; 160]
        };
        self.metrics.jitter_buffer_depth = self.packets.len();
        output
    }
    pub fn reset(&mut self) {
        self.packets.clear();
        self.seen.clear();
        self.expected = None;
        self.started = false;
        self.empty_ticks = 0;
        self.metrics.jitter_buffer_depth = 0;
    }
}
#[derive(Clone, Serialize)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}
struct Select {
    id: String,
    reply: std::sync::mpsc::Sender<Result<String, String>>,
}
#[derive(Clone)]
pub struct Playback {
    queue: Arc<ArrayQueue<f32>>,
    active: Arc<AtomicBool>,
    tx: std::sync::mpsc::SyncSender<Select>,
    pub state: Arc<Mutex<String>>,
    pub underruns: Arc<AtomicU64>,
    pub overruns: Arc<AtomicU64>,
    pub input_samples: Arc<AtomicU64>,
    pub output_frames: Arc<AtomicU64>,
    pub output_samples: Arc<AtomicU64>,
    pub callback_starvations: Arc<AtomicU64>,
    pub sample_rate: Arc<AtomicU64>,
    pub channels: Arc<AtomicU64>,
    opened: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}
impl Playback {
    pub fn outputs() -> Result<Vec<Output>> {
        let h = cpal::default_host();
        let default = h.default_output_device().and_then(|d| d.name().ok());
        Ok(h.output_devices()?
            .enumerate()
            .map(|(i, d)| {
                let name = d.name().unwrap_or_else(|_| "Unknown".into());
                Output {
                    id: i.to_string(),
                    is_default: Some(&name) == default.as_ref(),
                    name,
                }
            })
            .collect())
    }
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Select>(4);
        let p = Self {
            queue: Arc::new(ArrayQueue::new(1600)),
            active: Arc::new(AtomicBool::new(false)),
            tx,
            state: Arc::new(Mutex::new("stopped".into())),
            underruns: Arc::new(AtomicU64::new(0)),
            overruns: Arc::new(AtomicU64::new(0)),
            input_samples: Arc::new(AtomicU64::new(0)),
            output_frames: Arc::new(AtomicU64::new(0)),
            output_samples: Arc::new(AtomicU64::new(0)),
            callback_starvations: Arc::new(AtomicU64::new(0)),
            sample_rate: Arc::new(AtomicU64::new(0)),
            channels: Arc::new(AtomicU64::new(0)),
            opened: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
        };
        let mut worker = p.clone();
        // The worker must not retain a sender to its own command channel.
        let (unused_tx, unused_rx) = std::sync::mpsc::sync_channel(1);
        drop(unused_rx);
        worker.tx = unused_tx;
        std::thread::spawn(move || {
            let mut stream: Option<cpal::Stream> = None;
            while let Ok(command) = rx.recv() {
                if command.id == "__shutdown" {
                    let _ = command.reply.send(Ok("stopped".into()));
                    break;
                }
                worker.opened.store(false, Ordering::Relaxed);
                stream.take();
                worker.clear();
                match worker.open(&command.id) {
                    Ok((s, name)) => {
                        worker.opened.store(true, Ordering::Relaxed);
                        stream = Some(s);
                        *worker.state.lock().unwrap() = name.clone();
                        let _ = command.reply.send(Ok(name));
                    }
                    Err(e) => {
                        *worker.state.lock().unwrap() = format!("error: {e}");
                        let _ = command.reply.send(Err(e.to_string()));
                    }
                }
            }
        });
        p
    }
    pub fn select(&self, id: &str) -> Result<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx.try_send(Select {
            id: id.into(),
            reply: tx,
        })?;
        rx.recv_timeout(Duration::from_secs(5))?
            .map_err(|e| anyhow!(e))
    }
    pub fn shutdown(&self) {
        let _ = self.select("__shutdown");
    }
    pub fn clear(&self) {
        self.active.store(false, Ordering::Relaxed);
        while self.queue.pop().is_some() {}
    }
    pub fn set_mute(&self, mute: bool) {
        self.muted.store(mute, Ordering::Relaxed);
    }
    pub fn feed(&self, frame: [i16; 160]) {
        if !self.opened.load(Ordering::Relaxed) {
            return;
        }
        if self.queue.len() > 960 {
            self.clear();
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        for s in frame {
            if self.queue.push(s as f32 / 32768.0).is_err() {
                self.overruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.active.store(true, Ordering::Relaxed);
    }
    fn open(&self, id: &str) -> Result<(cpal::Stream, String)> {
        let host = cpal::default_host();
        let device = if id == "default" {
            host.default_output_device()
        } else {
            host.output_devices()?.nth(id.parse()?)
        }
        .ok_or_else(|| anyhow!("output not found"))?;
        let config = device.default_output_config()?;
        let stream_config = config.config();
        let name = device.name()?;
        self.sample_rate
            .store(stream_config.sample_rate.0 as u64, Ordering::Relaxed);
        self.channels
            .store(stream_config.channels as u64, Ordering::Relaxed);
        let s = match config.sample_format() {
            cpal::SampleFormat::F32 => self.build::<f32>(&device, &stream_config)?,
            cpal::SampleFormat::I16 => self.build::<i16>(&device, &stream_config)?,
            cpal::SampleFormat::U16 => self.build::<u16>(&device, &stream_config)?,
            other => return Err(anyhow!("unsupported output sample format: {other:?}")),
        };
        s.play()?;
        Ok((s, name))
    }
    fn build<T>(&self, d: &cpal::Device, c: &cpal::StreamConfig) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let queue = self.queue.clone();
        let active = self.active.clone();
        let underruns = self.underruns.clone();
        let state = self.state.clone();
        let channels = c.channels as usize;
        let input_samples = self.input_samples.clone();
        let output_frames = self.output_frames.clone();
        let output_samples = self.output_samples.clone();
        let starvations = self.callback_starvations.clone();
        let muted = self.muted.clone();

        let mut sampler = Resampler::new(c.sample_rate.0);
        Ok(d.build_output_stream(
            c,
            move |data: &mut [T], _| {
                output_frames.fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
                output_samples.fetch_add(data.len() as u64, Ordering::Relaxed);
                if !active.load(Ordering::Relaxed) || (!sampler.primed && queue.len() < 320) {
                    data.fill(T::from_sample(0.0));
                    sampler.reset();
                    return;
                }
                let mut short = false;
                for frame in data.chunks_mut(channels) {
                    let (sample, starved) = sampler.next(|| {
                        let next = queue.pop();
                        if next.is_some() {
                            input_samples.fetch_add(1, Ordering::Relaxed);
                        }
                        next
                    });
                    short |= starved;
                    for out in frame {
                        *out = T::from_sample(sample);
                    }
                }
                if short {
                    starvations.fetch_add(1, Ordering::Relaxed);
                    if !muted.load(Ordering::Relaxed) {
                        underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    sampler.reset();
                }
            },
            move |e| {
                *state.lock().unwrap() = format!("error: {e}");
            },
            None,
        )?)
    }
}
impl Default for Playback {
    fn default() -> Self {
        Self::new()
    }
}
pub struct Engine {
    pub jitter: Jitter,
    pub playback: Playback,
    started: Instant,
    bytes: u64,
    sequence: sequence::SequenceTracker,
}
impl Engine {
    pub fn new(depth: usize) -> Self {
        Self {
            jitter: Jitter::new(depth),
            playback: Playback::new(),
            started: Instant::now(),
            bytes: 0,
            sequence: sequence::SequenceTracker::default(),
        }
    }
    pub fn receive(&mut self, p: AudioPacket) {
        self.sequence
            .observe(p.sequence, self.started.elapsed().as_millis() as u64);
        self.bytes += 320;
        self.jitter.push(p)
    }
    pub fn tick(&mut self) {
        self.sequence
            .finalize(self.started.elapsed().as_millis() as u64);
        self.jitter.metrics.sequence_gaps = self.sequence.gaps;
        self.jitter.metrics.sequence_out_of_order = self.sequence.reordered;
        let was_started = self.jitter.started;
        let f = self.jitter.tick();
        if self.jitter.started || was_started {
            self.playback.feed(f);
        }
    }
    pub fn reset(&mut self) {
        self.sequence.reset();
        self.rebuffer();
    }
    pub fn set_mute(&mut self, mute: bool) {
        self.jitter.muted = mute;
        self.playback.set_mute(mute);
    }
    pub fn rebuffer(&mut self) {
        self.jitter.reset();
        self.playback.clear()
    }
    pub fn metrics(&self) -> Metrics {
        let mut m = self.jitter.metrics.clone();
        m.receive_bitrate =
            (self.bytes as f64 * 8.0 / self.started.elapsed().as_secs_f64().max(1.0)) as u64;
        m.buffer_underruns += self.playback.underruns.load(Ordering::Relaxed);
        m.buffer_overruns += self.playback.overruns.load(Ordering::Relaxed);
        m.output_state = self.playback.state.lock().unwrap().clone();
        m.input_samples_consumed = self.playback.input_samples.load(Ordering::Relaxed);
        m.output_frames_requested = self.playback.output_frames.load(Ordering::Relaxed);
        m.output_samples_requested = self.playback.output_samples.load(Ordering::Relaxed);
        m.callback_starvations = self.playback.callback_starvations.load(Ordering::Relaxed);
        m.ring_depth_samples = self.playback.queue.len();
        m.output_sample_rate = self.playback.sample_rate.load(Ordering::Relaxed);
        m.output_channels = self.playback.channels.load(Ordering::Relaxed);

        m
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn packet(n: u32) -> AudioPacket {
        AudioPacket {
            sequence: n,
            timestamp: n.wrapping_mul(160),
            session: [0; 16],
            samples: [n as i16; 160],
        }
    }
    #[test]
    fn reorder_loss_duplicate() {
        let mut j = Jitter::new(2);
        j.push(packet(0));
        j.push(packet(2));
        j.push(packet(1));
        j.push(packet(1));
        assert_eq!(j.tick()[0], 0);
        assert_eq!(j.tick()[0], 1);
        assert_eq!(j.tick()[0], 2);
        j.tick();
        assert_eq!(j.metrics.lost_packets, 1);
        assert_eq!(j.metrics.duplicate_packets, 1);
        assert!(j.metrics.out_of_order_packets > 0);
    }
    #[test]
    fn bounded_overflow() {
        let mut j = Jitter::new(2);
        for n in 0..1000 {
            j.push(packet(n))
        }
        assert!(j.packets.len() <= 8);
        assert!(j.metrics.buffer_overruns > 0);
    }
    #[test]
    fn jitter_wrap_and_underrun() {
        let mut j = Jitter::new(2);
        j.push(packet(u32::MAX));
        assert_eq!(j.tick(), [0; 160]);
        j.push(packet(0));
        assert_eq!(j.tick()[0], -1);
        assert_eq!(j.tick()[0], 0);
        j.tick();
        assert_eq!(j.metrics.lost_packets, 1);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn sender_stall_rebuffers_instead_of_permanent_silence() {
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
        for _ in 0..10 {
            j.tick();
        }
        j.push(packet(2));
        j.push(packet(3));
        assert_eq!(j.tick()[0], 123);
        assert_eq!(j.tick()[0], 123);
    }
}

#[cfg(test)]
mod resampling_tests {
    use super::*;
    #[test]
    fn sixteen_to_forty_eight_khz_preserves_440hz_and_duration() {
        let source: Vec<f32> = (0..16000)
            .map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / 16000.0).sin())
            .collect();
        let mut input = source.into_iter();
        let mut sampler = Resampler::new(48000);
        let output: Vec<f32> = (0..48000)
            .map(|_| sampler.next(|| input.next()).0)
            .collect();
        let crossings = output
            .windows(2)
            .filter(|s| s[0] <= 0.0 && s[1] > 0.0)
            .count();
        assert!((439..=441).contains(&crossings));
        assert!(output.iter().all(|s| s.abs() <= 1.0));
    }
    #[test]
    fn forty_four_point_one_khz_support() {
        let mut sampler = Resampler::new(44100);
        let mut n = 0;
        for _ in 0..44100 {
            let (sample, short) = sampler.next(|| {
                n += 1;
                Some(0.5)
            });
            assert!(!short);
            assert!((sample - 0.5).abs() < 0.00001);
        }
        assert!((16000..=16002).contains(&n));
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    fn packet(n: u32) -> AudioPacket {
        AudioPacket {
            sequence: n,
            timestamp: n * 160,
            session: [0; 16],
            samples: [1; 160],
        }
    }
    #[test]
    fn duplicates_late_drops_and_delivery_are_distinct() {
        let mut j = Jitter::new(2);
        j.push(packet(0));
        j.push(packet(1));
        j.push(packet(1));
        j.tick();
        j.tick();
        j.push(packet(2));
        j.push(packet(3));
        j.tick();
        j.tick();
        j.push(packet(0));
        assert_eq!(j.metrics.received_packets, 6);
        assert_eq!(j.metrics.accepted_packets, 4);
        assert_eq!(j.metrics.delivered_packets, 4);
        assert_eq!(j.metrics.delivered_samples, 640);
        assert_eq!(j.metrics.duplicate_packets, 2);
    }
    #[test]
    fn muted_missing_frame_is_a_deficit_not_an_audible_underrun() {
        let mut j = Jitter::new(2);
        j.muted = true;
        j.push(packet(0));
        j.push(packet(1));
        j.tick();
        j.tick();
        j.tick();
        assert_eq!(j.metrics.lost_packets, 1);
        assert_eq!(j.metrics.buffer_underruns, 0);
    }
    #[test]
    fn headless_engine_does_not_accumulate_playback_samples() {
        let mut engine = Engine::new(2);
        for n in 0..100 {
            engine.receive(packet(n));
            engine.tick();
        }
        assert_eq!(engine.metrics().ring_depth_samples, 0);
        assert_eq!(engine.metrics().buffer_overruns, 0);
        engine.playback.shutdown();
    }
}
