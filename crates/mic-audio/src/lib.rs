pub mod capture;
#[cfg(test)]
mod clock_amplification_tests;
#[cfg(test)]
mod clock_drift_tests;
#[cfg(test)]
mod continuity_regression;
pub mod declick;
pub mod diagnostics;
mod drift;
mod output;
mod sequence;
use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
pub use declick::Declicker;
/// Observes callback entry gaps and input starvation, not acoustic delivery.
struct CallbackMonitor {
    rate: u32,
    stream: u64,
    previous: Option<(Instant, u64)>,
    recovering: bool,
}
impl CallbackMonitor {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            stream: 0,
            previous: None,
            recovering: false,
        }
    }
    fn begin(
        &mut self,
        now: Instant,
        frames: usize,
        running: bool,
        ring: usize,
        d: &diagnostics::Diagnostics,
    ) {
        let stream = d.stream.load(Ordering::Acquire);
        if stream != self.stream {
            self.stream = stream;
            self.previous = None;
            self.recovering = false;
        }
        if let Some((previous, budget)) = self.previous
            && running
        {
            let gap = now.saturating_duration_since(previous).as_micros() as u64;
            if gap > budget + 20_000 {
                d.callback_delay(gap, budget, ring);
            }
        }
        self.previous = Some((now, frames as u64 * 1_000_000 / self.rate as u64));
    }
    fn finish(&mut self, short: bool, ring: usize, d: &diagnostics::Diagnostics) {
        let event = if short {
            Some((
                diagnostics::EventType::AudioCallbackStarvation,
                "resampler_input_empty",
            ))
        } else if self.recovering {
            Some((
                diagnostics::EventType::AudioRecovery,
                "callback_consumption_resumed",
            ))
        } else {
            None
        };
        if let Some((kind, reason)) = event {
            d.emit(
                kind,
                None,
                None,
                d.depth.load(Ordering::Relaxed) as usize,
                d.target.load(Ordering::Relaxed) as usize,
                Some(ring),
                reason,
            );
        }
        self.recovering = short;
    }
}
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
    a_faded: bool,
    b_faded: bool,
}
impl Resampler {
    fn new(rate: u32) -> Self {
        Self {
            step: 16000.0 / rate as f64,
            phase: 0.0,
            a: 0.0,
            b: 0.0,
            primed: false,
            a_faded: false,
            b_faded: false,
        }
    }
    fn reset(&mut self) {
        self.primed = false;
        self.phase = 0.0;
    }
    #[cfg(test)]
    fn next(&mut self, mut input: impl FnMut() -> Option<f32>) -> (f32, bool) {
        let (value, short, _) = self.next_tagged(|| {
            input().map(|value| output::Input {
                value,
                upstream_faded: false,
                epoch: 0,
            })
        });
        (value, short)
    }
    fn next_tagged(
        &mut self,
        mut input: impl FnMut() -> Option<output::Input>,
    ) -> (f32, bool, bool) {
        let mut short = false;
        let mut take = || {
            input().unwrap_or_else(|| {
                short = true;
                output::Input {
                    value: 0.0,
                    upstream_faded: false,
                    epoch: 0,
                }
            })
        };
        if !self.primed {
            let a = take();
            let b = take();
            self.a = a.value;
            self.a_faded = a.upstream_faded;
            self.b = b.value;
            self.b_faded = b.upstream_faded;
            self.primed = true;
        }
        let sample = self.a + (self.b - self.a) * self.phase as f32;
        let faded = self.a_faded || self.b_faded;
        self.phase += self.step;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.a = self.b;
            self.a_faded = self.b_faded;
            let b = take();
            self.b = b.value;
            self.b_faded = b.upstream_faded;
        }
        (sample, short, faded)
    }
}
#[derive(Default, Clone, Serialize, Debug)]
pub struct Metrics {
    pub output_diagnostics: output::OutputSnapshot,
    pub rx_authenticated_packet_age_ms: Option<f64>,
    pub jitter_missing_samples_fed: u64,
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
    pub target_buffer_depth: usize,
    pub contiguous_playable_depth: usize,
    pub underrun_fade_outs: u64,
    pub recovery_fade_ins: u64,
    pub inter_arrival_jitter_ms: f64,
    pub inter_arrival_interval_ms: Option<f64>,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct IsolationOptions {
    pub output_fade: bool,
    pub capacity_policy: bool,
    pub recovery_policy: bool,
}
impl Default for IsolationOptions {
    fn default() -> Self {
        Self {
            output_fade: true,
            capacity_policy: true,
            recovery_policy: true,
        }
    }
}
impl IsolationOptions {
    pub fn from_env() -> Result<Self> {
        fn flag(name: &str) -> Result<bool> {
            match std::env::var(name) {
                Err(std::env::VarError::NotPresent) => Ok(true),
                Ok(v) if v == "on" => Ok(true),
                Ok(v) if v == "off" => Ok(false),
                _ => Err(anyhow!("{name} must be on or off")),
            }
        }
        Ok(Self {
            output_fade: flag("MIC_TEST_OUTPUT_FADE")?,
            capacity_policy: flag("MIC_TEST_CAPACITY_POLICY")?,
            recovery_policy: flag("MIC_TEST_RECOVERY_POLICY")?,
        })
    }
}
pub struct Jitter {
    capacity_policy: bool,
    packets: HashMap<u32, AudioPacket>,
    expected: Option<u32>,
    pub started: bool,
    pub depth: usize,
    pub metrics: Metrics,
    seen: std::collections::VecDeque<u32>,
    empty_ticks: u8,
    pub muted: bool,
    pub last_tick_missing: bool,
    diagnostics: Option<Arc<diagnostics::Diagnostics>>,
    recovering: bool,
}
impl Jitter {
    pub fn new(depth: usize) -> Self {
        let depth = depth.clamp(2, 4);
        Self {
            capacity_policy: true,
            packets: HashMap::new(),
            expected: None,
            started: false,
            depth,
            metrics: Metrics {
                target_buffer_depth: depth,
                ..Metrics::default()
            },
            seen: std::collections::VecDeque::new(),
            empty_ticks: 0,
            muted: false,
            last_tick_missing: false,
            diagnostics: None,
            recovering: false,
        }
    }
    fn event(
        &self,
        kind: diagnostics::EventType,
        packet: Option<u32>,
        expected: Option<u32>,
        reason: &'static str,
    ) {
        if let Some(d) = &self.diagnostics {
            d.emit(
                kind,
                packet,
                expected,
                self.packets.len(),
                self.depth,
                None,
                reason,
            );
        }
    }
    /// Occupancy is stored packets; contiguous depth starts exactly at expected.
    pub fn occupancy(&self) -> usize {
        self.packets.len()
    }
    pub fn expected_sequence(&self) -> Option<u32> {
        self.expected
    }
    pub fn contiguous_playable_depth(&self) -> usize {
        let Some(expected) = self.expected else {
            return 0;
        };
        (0..self.packets.len())
            .take_while(|offset| {
                self.packets
                    .contains_key(&expected.wrapping_add(*offset as u32))
            })
            .count()
    }
    pub fn push(&mut self, p: AudioPacket) {
        let sequence = p.sequence;
        let source_timestamp = p.timestamp;
        let before = self.occupancy();
        let accepted = self.metrics.accepted_packets;
        let cursor_before = self.expected;
        let started_before = self.started;
        self.push_inner(p);
        let anchor_changed = (self.expected != cursor_before && (!started_before || !self.started))
            || (started_before && !self.started);
        if let Some(d) = &self.diagnostics {
            if anchor_changed {
                d.playout_epoch.fetch_add(1, Ordering::Relaxed);
            }
            let mut o = d.observation("jitter_enqueue");
            if anchor_changed {
                o.reason = Some("playout_anchor_changed");
            }
            o.packet_sequence = Some(sequence);
            if d.drift_enabled {
                o.source_sample_index = Some(source_timestamp);
            }
            o.expected_sequence = self.expected;
            o.occupancy = Some(self.occupancy());
            o.occupancy_before = Some(before);
            o.contiguous_depth = Some(self.contiguous_playable_depth());
            o.capacity = Some(8);
            o.late_packet_count = Some(self.metrics.late_packet_drops);
            o.underrun_count = Some(self.metrics.buffer_underruns);
            o.admitted = Some(self.metrics.accepted_packets > accepted);
            d.observe(o);
        }
    }
    fn push_inner(&mut self, p: AudioPacket) {
        if !self.capacity_policy {
            self.push_legacy(p);
            return;
        }
        self.metrics.received_packets += 1;
        if self.seen.contains(&p.sequence) || self.packets.contains_key(&p.sequence) {
            self.metrics.duplicate_packets += 1;
            return;
        }
        if let Some(e) = self.expected {
            let distance = p.sequence.wrapping_sub(e);
            if ahead(e, p.sequence) {
                self.metrics.out_of_order_packets += 1;
                if !self.started && e.wrapping_sub(p.sequence) < 8 {
                    self.expected = Some(p.sequence);
                } else {
                    self.metrics.late_packet_drops += 1;
                    self.event(
                        diagnostics::EventType::LatePacket,
                        Some(p.sequence),
                        Some(e),
                        "behind_playout_cursor",
                    );
                    return;
                }
            } else if distance > 100 {
                // Includes ambiguous half-range ordering. Preserve the playable
                // timeline; an empty-stall reset can establish a new epoch later.
                self.metrics.jitter_buffer_drops += 1;
                self.event(
                    diagnostics::EventType::BufferResync,
                    Some(p.sequence),
                    Some(e),
                    "forward_sequence_rejected",
                );
                return;
            } else if p.sequence != e.wrapping_add(self.packets.len() as u32) {
                self.metrics.out_of_order_packets += 1;
            }
        } else {
            self.expected = Some(p.sequence);
        }
        if self.packets.len() >= 8 {
            let expected = self.expected.unwrap();
            let farthest = *self
                .packets
                .keys()
                .max_by_key(|seq| seq.wrapping_sub(expected))
                .unwrap();
            self.metrics.buffer_overruns += 1;
            self.metrics.jitter_buffer_drops += 1;
            if p.sequence.wrapping_sub(expected) < farthest.wrapping_sub(expected) {
                self.event(
                    diagnostics::EventType::BufferResync,
                    Some(p.sequence),
                    Some(expected),
                    "jitter_capacity_replace_farthest",
                );
                self.packets.remove(&farthest);
                self.seen.retain(|seq| *seq != farthest);
            } else {
                self.event(
                    diagnostics::EventType::BufferResync,
                    Some(p.sequence),
                    Some(expected),
                    "jitter_capacity_reject_farther",
                );
                return;
            }
        }
        self.seen.push_back(p.sequence);
        if self.seen.len() > 64 {
            self.seen.pop_front();
        }
        self.metrics.accepted_packets += 1;
        self.packets.insert(p.sequence, p);
        self.metrics.jitter_buffer_depth = self.packets.len();
        self.metrics.contiguous_playable_depth = self.contiguous_playable_depth();
    }
    fn push_legacy(&mut self, p: AudioPacket) {
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
                    self.event(
                        diagnostics::EventType::LatePacket,
                        Some(p.sequence),
                        Some(e),
                        "behind_playout_cursor",
                    );
                    return;
                }
            } else if p.sequence != e.wrapping_add(self.packets.len() as u32) {
                self.metrics.out_of_order_packets += 1;
            }
        } else {
            self.expected = Some(p.sequence)
        }
        if self.packets.len() >= 8 {
            self.event(
                diagnostics::EventType::BufferResync,
                Some(p.sequence),
                self.expected,
                "jitter_capacity",
            );
            self.recovering = true;
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
            self.event(
                diagnostics::EventType::BufferResync,
                Some(p.sequence),
                self.expected,
                "forward_sequence_jump",
            );
            self.recovering = true;
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
        let expected = self.expected;
        let before = self.occupancy();
        let delivered = self.metrics.delivered_packets;
        let was_started = self.started;
        let output = self.tick_inner();
        if let Some(d) = &self.diagnostics {
            let mut o = d.observation("jitter_dequeue");
            o.expected_sequence = expected;
            o.occupancy = Some(self.occupancy());
            o.occupancy_before = Some(before);
            o.contiguous_depth = Some(self.contiguous_playable_depth());
            o.capacity = Some(8);
            o.late_packet_count = Some(self.metrics.late_packet_drops);
            o.underrun_count = Some(self.metrics.buffer_underruns);
            o.delivered = Some(self.metrics.delivered_packets > delivered);
            o.started = Some(self.started);
            o.operational = Some(was_started || self.started);
            d.observe(o);
        }
        output
    }
    fn tick_inner(&mut self) -> [i16; 160] {
        if !self.started {
            if self.packets.len() < self.depth {
                self.last_tick_missing = true;
                return [0; 160];
            };
            self.started = true;
        }
        let e = self.expected.unwrap();
        self.expected = Some(e.wrapping_add(1));
        let output = if let Some(p) = self.packets.remove(&e) {
            if self.recovering {
                self.event(
                    diagnostics::EventType::AudioRecovery,
                    Some(e),
                    Some(e),
                    "jitter_packet_delivery_resumed",
                );
                self.recovering = false;
            }
            self.last_tick_missing = false;
            self.empty_ticks = 0;
            self.metrics.delivered_packets += 1;
            self.metrics.delivered_samples += 160;
            p.samples
        } else {
            self.last_tick_missing = true;
            self.metrics.lost_packets += 1;
            self.event(
                diagnostics::EventType::BufferUnderrun,
                None,
                Some(e),
                if self.muted {
                    "muted_playout_deadline_missing"
                } else {
                    "playout_deadline_missing"
                },
            );
            self.recovering = true;
            if !self.muted {
                self.metrics.buffer_underruns += 1;
            }
            self.empty_ticks += 1;
            if self.empty_ticks >= 3 && self.packets.is_empty() {
                self.event(
                    diagnostics::EventType::BufferResync,
                    None,
                    self.expected,
                    "three_empty_deadlines",
                );
                // A stalled sender/scheduler must not leave the playout cursor ahead forever.
                self.started = false;
                self.expected = None;
                if let Some(d) = &self.diagnostics {
                    d.playout_epoch.fetch_add(1, Ordering::Relaxed);
                }
                self.empty_ticks = 0;
            }
            [0; 160]
        };
        self.metrics.jitter_buffer_depth = self.packets.len();
        self.metrics.contiguous_playable_depth = self.contiguous_playable_depth();
        output
    }
    pub fn reset(&mut self) {
        if let Some(d) = &self.diagnostics {
            d.playout_epoch.fetch_add(1, Ordering::Relaxed);
        }
        self.recovering = false;
        self.packets.clear();
        self.seen.clear();
        self.expected = None;
        self.started = false;
        self.empty_ticks = 0;
        self.metrics.jitter_buffer_depth = 0;
        self.metrics.contiguous_playable_depth = 0;
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
    pub isolation: IsolationOptions,
    output_diagnostics: Arc<output::Counters>,
    queue: Arc<ArrayQueue<output::Input>>,
    epoch: Arc<AtomicU64>,
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
    pub diagnostics: Arc<diagnostics::Diagnostics>,
    pub capture: Arc<capture::Capture>,
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
        Self::with_options(IsolationOptions::default())
    }
    pub fn with_options(isolation: IsolationOptions) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Select>(4);
        let diagnostics = diagnostics::Diagnostics::new();
        let capture = capture::Capture::new(diagnostics.clone());
        let p = Self {
            isolation,
            output_diagnostics: Arc::new(output::Counters::default()),
            queue: Arc::new(ArrayQueue::new(1600)),
            epoch: Arc::new(AtomicU64::new(0)),
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
            diagnostics,
            capture,
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
        let _ = self.capture.stop();
        let _ = self.select("__shutdown");
    }
    pub fn clear(&self) {
        self.active.store(false, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let mut discarded = 0;
        while self.queue.pop().is_some() {
            discarded += 1;
        }
        if self.diagnostics.drift_enabled {
            let mut o = self.diagnostics.observation("playback_clear");
            o.output_epoch = Some(self.epoch.load(Ordering::Acquire));
            o.discarded_samples = Some(discarded);
            self.diagnostics.observe(o);
        }
    }
    pub fn set_mute(&self, mute: bool) {
        self.muted.store(mute, Ordering::Relaxed);
    }
    pub fn feed(&self, frame: [i16; 160]) {
        self.feed_tagged(frame, false);
    }
    fn feed_tagged(&self, frame: [i16; 160], upstream_faded: bool) {
        if !self.opened.load(Ordering::Relaxed) {
            return;
        }
        if self.queue.len() > 960 {
            self.diagnostics.emit(
                diagnostics::EventType::BufferResync,
                None,
                None,
                self.diagnostics.depth.load(Ordering::Relaxed) as usize,
                self.diagnostics.target.load(Ordering::Relaxed) as usize,
                Some(self.queue.len()),
                "playback_ring_backlog_clear",
            );
            self.clear();
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        self.capture.feed(frame);
        let epoch = self.epoch.load(Ordering::Acquire);
        let mut dropped = 0;
        for s in frame {
            if self
                .queue
                .push(output::Input {
                    value: s as f32 / 32768.0,
                    upstream_faded,
                    epoch,
                })
                .is_err()
            {
                self.overruns.fetch_add(1, Ordering::Relaxed);
                dropped += 1;
            }
        }
        self.active.store(true, Ordering::Release);
        let mut o = self.diagnostics.observation("playback_feed");
        o.ring_after = Some(self.queue.len());
        o.capacity = Some(self.queue.capacity());
        if self.diagnostics.drift_enabled {
            o.output_epoch = Some(epoch);
            o.accepted_samples = Some(160 - dropped);
            o.dropped_samples = Some(dropped);
        }
        self.diagnostics.observe(o);
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
        let epoch = self.epoch.clone();
        let active = self.active.clone();
        let underruns = self.underruns.clone();
        let state = self.state.clone();
        let channels = c.channels as usize;
        let input_samples = self.input_samples.clone();
        let output_frames = self.output_frames.clone();
        let output_samples = self.output_samples.clone();
        let starvations = self.callback_starvations.clone();
        let muted = self.muted.clone();
        let diagnostics = self.diagnostics.clone();

        let error_diagnostics = diagnostics.clone();
        let output_rate = c.sample_rate.0;
        let mut monitor = CallbackMonitor::new(output_rate);
        let mut renderer = output::Renderer::with_options(output_rate, self.isolation);
        let counters = self.output_diagnostics.clone();
        Ok(d.build_output_stream(
            c,
            move |data: &mut [T], _| {
                let now = Instant::now();
                let mut observation = diagnostics.observation("audio_callback");
                observation.interval_us = monitor
                    .previous
                    .map(|(t, _)| now.saturating_duration_since(t).as_micros() as u64);
                observation.budget_us = monitor.previous.map(|(_, budget)| budget);
                if monitor.stream != observation.stream_id {
                    observation.interval_us = None;
                    observation.budget_us = None;
                }
                observation.ring_before = Some(queue.len());
                if diagnostics.drift_enabled {
                    observation.output_epoch = Some(epoch.load(Ordering::Acquire));
                }
                let running = active.load(Ordering::Relaxed) && renderer.primed();
                observation.started = Some(running);
                counters.callback(diagnostics.now_us(), output_rate);
                monitor.begin(
                    now,
                    data.len() / channels,
                    running,
                    queue.len(),
                    &diagnostics,
                );
                output_frames.fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
                output_samples.fetch_add(data.len() as u64, Ordering::Relaxed);
                let stats = renderer.render(data, channels, &queue, &active, &epoch);
                observation.requested_frames = Some((data.len() / channels) as u64);
                observation.supplied_frames = Some(stats.supplied_frames);
                observation.inserted_frames = Some(stats.inserted_frames);
                observation.starvation_frames = Some(stats.starvation_frames);
                observation.prefill_frames = Some(stats.prefill_frames);
                observation.inactive_frames = Some(stats.inactive_frames);
                observation.missing_input_samples = Some(stats.missing_input_samples);
                observation.consumed_input_samples = Some(stats.consumed);
                observation.output_rate = Some(output_rate);
                observation.ring_after = Some(queue.len());
                if diagnostics.drift_enabled {
                    observation.output_epoch_end = Some(epoch.load(Ordering::Acquire));
                    observation.discarded_samples = Some(stats.stale_discarded);
                    observation.resampler_phase = Some(renderer.phase());
                    observation.resampler_step = Some(renderer.step());
                }
                diagnostics.observe(observation);
                counters.record(&stats);
                input_samples.fetch_add(stats.consumed, Ordering::Relaxed);
                if stats.rendered {
                    monitor.finish(stats.short, queue.len(), &diagnostics);
                }
                if stats.short {
                    starvations.fetch_add(1, Ordering::Relaxed);
                    if !muted.load(Ordering::Relaxed) {
                        underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            move |e| {
                error_diagnostics.emit(
                    diagnostics::EventType::AudioOutputError,
                    None,
                    None,
                    error_diagnostics.depth.load(Ordering::Relaxed) as usize,
                    error_diagnostics.target.load(Ordering::Relaxed) as usize,
                    None,
                    "cpal_output_error_see_output_state",
                );
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
    pub declicker: Declicker,
    started: Instant,
    bytes: u64,
    sequence: sequence::SequenceTracker,
    last_arrival: Option<Instant>,
    inter_arrival_jitter_ms: f64,
    last_inter_arrival_interval_ms: Option<f64>,
    last_timestamp: Option<u32>,
    diagnostic_arrival_us: Option<u64>,
}
impl Engine {
    pub fn new(depth: usize) -> Self {
        Self::with_options(depth, IsolationOptions::default())
    }
    pub fn with_options(depth: usize, options: IsolationOptions) -> Self {
        let playback = Playback::with_options(options);
        let mut jitter = Jitter::new(depth);
        jitter.capacity_policy = options.capacity_policy;
        jitter.diagnostics = Some(playback.diagnostics.clone());
        Self {
            jitter,
            playback,
            declicker: Declicker::new(),
            started: Instant::now(),
            bytes: 0,
            sequence: sequence::SequenceTracker::default(),
            last_arrival: None,
            inter_arrival_jitter_ms: 0.0,
            last_inter_arrival_interval_ms: None,
            last_timestamp: None,
            diagnostic_arrival_us: None,
        }
    }
    pub fn inter_arrival_jitter_ms(&self) -> f64 {
        self.inter_arrival_jitter_ms
    }
    pub fn target_buffer_depth(&self) -> usize {
        self.jitter.depth
    }
    pub fn underrun_fade_outs(&self) -> u64 {
        self.declicker.fade_outs
    }
    pub fn recovery_fade_ins(&self) -> u64 {
        self.declicker.fade_ins
    }
    pub fn receive(&mut self, p: AudioPacket) {
        let arrival_us = self.playback.diagnostics.now_us();
        self.receive_at(p, arrival_us);
    }
    /// Timestamp at socket receive completion, before authentication/decode work.
    pub fn receive_at(&mut self, p: AudioPacket, arrival_us: u64) {
        let now = Instant::now();
        if let (Some(prev_time), Some(prev_ts)) = (self.last_arrival, self.last_timestamp) {
            let delta_ms = now.duration_since(prev_time).as_secs_f64() * 1000.0;
            let nominal_ms = p.timestamp.wrapping_sub(prev_ts) as i32 as f64 / 16.0;
            let d = (delta_ms - nominal_ms).abs();
            self.inter_arrival_jitter_ms += (d - self.inter_arrival_jitter_ms) / 16.0;
            self.last_inter_arrival_interval_ms = Some(delta_ms);
        }
        self.last_arrival = Some(now);
        self.last_timestamp = Some(p.timestamp);

        let reordered = self.sequence.reordered;
        let duplicates = self.sequence.duplicates;
        self.sequence
            .observe(p.sequence, self.started.elapsed().as_millis() as u64);
        let d = &self.playback.diagnostics;
        let mut o = d.observation("udp_receive");
        o.rx_monotonic_us = arrival_us;
        o.packet_sequence = Some(p.sequence);
        if d.drift_enabled {
            o.source_sample_index = Some(p.timestamp);
        }
        o.interval_us = self
            .diagnostic_arrival_us
            .map(|t| arrival_us.saturating_sub(t));
        o.sequence_gaps = Some(self.sequence.gaps);
        o.pending_sequence_holes = Some(self.sequence.pending_holes());
        o.out_of_order = Some(self.sequence.reordered > reordered);
        o.duplicate = Some(self.sequence.duplicates > duplicates);
        d.observe(o);
        self.diagnostic_arrival_us = Some(arrival_us);
        self.bytes += 320;
        self.jitter.push(p);
        self.update_diagnostic_depth();
    }
    fn update_diagnostic_depth(&self) {
        self.playback
            .diagnostics
            .depth
            .store(self.jitter.packets.len() as u64, Ordering::Relaxed);
        self.playback
            .diagnostics
            .target
            .store(self.jitter.depth as u64, Ordering::Relaxed);
    }
    pub fn set_context(&self, device: &str, session: &str) {
        self.playback.diagnostics.set_context(device, session);
    }
    pub fn tick(&mut self) {
        self.sequence
            .finalize(self.started.elapsed().as_millis() as u64);
        self.jitter.metrics.sequence_gaps = self.sequence.gaps;
        self.jitter.metrics.sequence_out_of_order = self.sequence.reordered;
        let mut o = self.playback.diagnostics.observation("sequence_status");
        o.sequence_gaps = Some(self.sequence.gaps);
        o.pending_sequence_holes = Some(self.sequence.pending_holes());
        self.playback.diagnostics.observe(o);
        let was_started = self.jitter.started;
        let f = self.jitter.tick();
        self.update_diagnostic_depth();
        let upstream_faded = self.jitter.last_tick_missing || self.declicker.in_underrun;
        let smoothed = self.declicker.process(f, self.jitter.last_tick_missing);
        if self.jitter.started || was_started {
            if self.jitter.last_tick_missing {
                self.jitter.metrics.jitter_missing_samples_fed += 160;
            }
            self.playback.feed_tagged(smoothed, upstream_faded);
        }
    }
    pub fn reset(&mut self) {
        self.playback
            .diagnostics
            .observe(self.playback.diagnostics.observation("stream_reset"));
        self.diagnostic_arrival_us = None;
        self.sequence.reset();
        self.last_arrival = None;
        self.last_timestamp = None;
        self.jitter.reset();
        self.playback.clear();
        self.declicker.reset();
        self.update_diagnostic_depth();
    }
    pub fn set_mute(&mut self, mute: bool) {
        self.jitter.muted = mute;
        self.playback.set_mute(mute);
    }
    pub fn rebuffer(&mut self) {
        self.jitter.event(
            diagnostics::EventType::BufferResync,
            None,
            self.jitter.expected,
            "receiver_scheduler_stall",
        );
        self.jitter.reset();
        self.jitter.recovering = true;
        self.update_diagnostic_depth();
        self.playback.clear();
        self.declicker.reset();
    }
    pub fn metrics(&self) -> Metrics {
        let mut m = self.jitter.metrics.clone();
        m.output_diagnostics = self.playback.output_diagnostics.snapshot();
        m.rx_authenticated_packet_age_ms = self
            .last_arrival
            .map(|t| t.elapsed().as_secs_f64() * 1000.0);
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
        m.target_buffer_depth = self.jitter.depth;
        m.contiguous_playable_depth = self.jitter.contiguous_playable_depth();
        m.underrun_fade_outs = self.declicker.fade_outs;
        m.recovery_fade_ins = self.declicker.fade_ins;
        m.inter_arrival_jitter_ms = self.inter_arrival_jitter_ms;
        m.inter_arrival_interval_ms = self.last_inter_arrival_interval_ms;

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
    fn timeline_observes_admission_loss_burst_and_recovery_without_changing_pcm() {
        let d = diagnostics::Diagnostics::bounded(128);
        d.set_context("tx", "session");
        let mut observed = Jitter::new(2);
        observed.diagnostics = Some(d.clone());
        let mut plain = Jitter::new(2);
        // Normal, single/contiguous missing, reordering, duplicate, bursts, delayed packet.
        for batch in [
            vec![0, 1],
            vec![3, 2, 2],
            vec![5],
            vec![],
            vec![],
            vec![4, 8, 10, 9, 11, 12, 13, 14, 15, 16, 17],
            vec![],
        ] {
            for n in batch {
                observed.push(packet(n));
                plain.push(packet(n));
            }
            assert_eq!(observed.tick(), plain.tick());
        }
        for _ in 0..15 {
            assert_eq!(observed.tick(), plain.tick());
        }
        let rows = d.drain_observations();
        assert!(
            rows.iter()
                .any(|r| r["record_type"] == "jitter_enqueue" && r["admitted"] == false)
        );
        assert!(rows.iter().any(|r| r["record_type"] == "jitter_dequeue"
            && r["delivered"] == false
            && r["started"] == true));
        assert!(rows.iter().all(|r| r["session_id"] == "session"));
        assert!(
            rows.iter()
                .filter(|r| r["capacity"] == 8)
                .all(|r| r["occupancy"].as_u64().unwrap() <= 8)
        );
        d.set_enabled(false);
        observed.reset();
        plain.reset();
        for n in 0..10 {
            observed.push(packet(n));
            plain.push(packet(n));
            assert_eq!(observed.tick(), plain.tick());
        }
        assert!(d.drain_observations().is_empty());
    }
    #[test]
    fn udp_timeline_preserves_socket_timestamp_duplicates_and_reordering() {
        let mut e = Engine::new(2);
        let d = diagnostics::Diagnostics::bounded(128);
        e.playback.diagnostics = d.clone();
        e.jitter.diagnostics = Some(d.clone());
        e.set_context("tx", "one");
        for (sequence, time) in [(0, 100), (2, 10100), (1, 20100), (1, 20101)] {
            e.receive_at(packet(sequence), time);
        }
        let rows = d.drain_observations();
        let arrivals: Vec<_> = rows
            .iter()
            .filter(|r| r["record_type"] == "udp_receive")
            .collect();
        assert_eq!(arrivals.len(), 4);
        assert_eq!(arrivals[0]["rx_monotonic_us"], 100);
        assert!(arrivals[0]["interval_us"].is_null());
        assert_eq!(arrivals[1]["interval_us"], 10000);
        assert_eq!(arrivals[2]["out_of_order"], true);
        assert_eq!(arrivals[3]["duplicate"], true);
        e.reset();
        e.set_context("tx", "two");
        e.receive_at(packet(0), 50000);
        let rows = d.drain_observations();
        let arrival = rows
            .iter()
            .find(|r| r["record_type"] == "udp_receive")
            .unwrap();
        assert_eq!(arrival["session_id"], "two");
        assert!(arrival["interval_us"].is_null());
        e.playback.shutdown();
    }
    #[test]
    fn diagnostic_normal_delay_loss_resync_and_disabled_equivalence() {
        use diagnostics::EventType::*;
        let d = diagnostics::Diagnostics::bounded(64);
        let mut j = Jitter::new(2);
        j.diagnostics = Some(d.clone());
        j.push(packet(0));
        j.push(packet(1));
        for n in 2..20 {
            j.tick();
            j.push(packet(n));
        }
        assert!(
            d.queue.is_empty(),
            "normal playout must not report discontinuities"
        );
        j.tick();
        j.tick();
        j.tick(); // seq 20 absent at its playout deadline
        j.push(packet(20)); // delayed packet is behind cursor
        j.push(packet(21));
        j.tick(); // recovery
        j.tick();
        j.tick();
        j.tick(); // three empty deadlines -> resync
        let records = d.drain();
        for kind in [BufferUnderrun, LatePacket, BufferResync, AudioRecovery] {
            assert!(
                records.iter().any(|r| r.event.event_type == kind),
                "missing {kind:?}"
            );
        }
        let late = records
            .iter()
            .find(|r| r.event.event_type == LatePacket)
            .unwrap();
        assert_eq!(late.event.packet_sequence, Some(20));
        assert_eq!(late.event.expected_sequence, Some(21));
        assert!(
            records
                .windows(2)
                .all(|r| r[0].event.rx_monotonic_us <= r[1].event.rx_monotonic_us)
        );
        j.push(packet(22));
        j.push(packet(23));
        j.tick();
        j.push(packet(200));
        assert!(
            d.drain()
                .iter()
                .any(|r| r.event.reason == "forward_sequence_rejected")
        );
        for n in 24..34 {
            j.push(packet(n));
        }
        assert!(
            d.drain()
                .iter()
                .any(|r| r.event.reason.starts_with("jitter_capacity_"))
        );
        d.set_enabled(false);
        let mut plain = Jitter::new(2);
        j.reset();
        for n in 0..20 {
            j.push(packet(n));
            plain.push(packet(n));
            assert_eq!(j.tick(), plain.tick());
        }
        for _ in 0..5 {
            assert_eq!(j.tick(), plain.tick());
        }
        assert!(d.queue.is_empty());
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
    fn callback_monitor_normal_gap_starvation_and_recovery() {
        let d = diagnostics::Diagnostics::bounded(16);
        let mut monitor = CallbackMonitor::new(48000);
        let now = Instant::now();
        monitor.begin(now, 480, false, 320, &d);
        monitor.finish(false, 160, &d);
        monitor.begin(now + Duration::from_millis(10), 480, true, 160, &d);
        monitor.finish(false, 0, &d);
        assert!(d.queue.is_empty());
        monitor.begin(now + Duration::from_millis(100), 480, true, 0, &d);
        monitor.finish(true, 0, &d);
        monitor.begin(now + Duration::from_millis(110), 480, false, 0, &d);
        // Inactive/rebuffering silence does not imply consumption recovery.
        monitor.begin(now + Duration::from_millis(500), 480, false, 320, &d);
        monitor.finish(false, 160, &d);
        let events = d.drain();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].event.event_type,
            diagnostics::EventType::AudioCallbackDelay
        );
        assert_eq!(events[0].event.callback_interval_us, Some(90000));
        assert_eq!(events[0].event.callback_budget_us, Some(10000));
        assert_eq!(
            events[1].event.event_type,
            diagnostics::EventType::AudioCallbackStarvation
        );
        assert_eq!(events[2].event.reason, "callback_consumption_resumed");
        monitor.finish(true, 0, &d);
        d.drain();
        d.set_context("new-tx", "new-session");
        monitor.begin(now + Duration::from_secs(3), 480, true, 320, &d);
        monitor.finish(false, 160, &d);
        assert!(
            d.queue.is_empty(),
            "stream restart is not callback recovery or a callback gap"
        );
        let mut sampler = Resampler::new(48000);
        assert!(
            sampler.next(|| None).1,
            "initial priming misses must be observed"
        );
    }
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
    #[test]
    fn engine_timing_diagnostics_and_continuity_metrics() {
        let mut engine = Engine::new(2);
        for n in 0..10 {
            engine.receive(packet(n));
            engine.tick();
        }
        assert_eq!(engine.target_buffer_depth(), 2);
        // Inter-arrival jitter is measured across received packets
        let jitter = engine.inter_arrival_jitter_ms();
        assert!(jitter >= 0.0);

        // Cause an underrun by ticking until buffer is exhausted
        engine.tick(); // drains last buffered packet
        engine.tick(); // missing frame triggers declick fade-out
        assert!(engine.underrun_fade_outs() > 0);

        // Resume packet stream and tick
        engine.receive(packet(10));
        engine.tick();
        assert!(engine.recovery_fade_ins() > 0);

        engine.playback.shutdown();
    }
    #[test]
    fn reordered_timestamps_do_not_create_unsigned_jitter_spikes() {
        let mut engine = Engine::new(2);
        for n in [0, 2, 1, 3] {
            engine.receive(packet(n));
        }
        // A backward 160-sample timestamp is -10 ms, not a 2^32-sample jump.
        assert!(engine.inter_arrival_jitter_ms() < 1000.0);
        engine.reset();
        for n in [u32::MAX / 160, (u32::MAX / 160) + 1] {
            let mut p = packet(0);
            p.sequence = n;
            p.timestamp = n.wrapping_mul(160);
            engine.receive(p);
        }
        assert!(engine.inter_arrival_jitter_ms() < 1000.0);
        engine.playback.shutdown();
    }
}
