//! Bounded, nonblocking producers; disk work belongs exclusively to the worker.
use crossbeam_queue::ArrayQueue;
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    BufferUnderrun,
    LatePacket,
    BufferResync,
    AudioRecovery,
    AudioCallbackStarvation,
    AudioCallbackDelay,
    AudioOutputError,
    FecRecovered,
    FecUnrecoverable,
    FecTooLate,
    FecParityReceived,
    TransportConfigApplied,
    TransportConfigRejected,
}
pub const EVENT_TYPES: [EventType; 13] = [
    EventType::BufferUnderrun,
    EventType::LatePacket,
    EventType::BufferResync,
    EventType::AudioRecovery,
    EventType::AudioCallbackStarvation,
    EventType::AudioCallbackDelay,
    EventType::AudioOutputError,
    EventType::FecRecovered,
    EventType::FecUnrecoverable,
    EventType::FecTooLate,
    EventType::FecParityReceived,
    EventType::TransportConfigApplied,
    EventType::TransportConfigRejected,
];
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Event {
    pub deadline_sequence: Option<u32>,
    pub playout_epoch: u64,
    pub event_type: EventType,
    pub rx_monotonic_us: u64,
    pub stream_id: u64,
    pub packet_sequence: Option<u32>,
    pub expected_sequence: Option<u32>,
    pub jitter_buffer_depth: usize,
    pub target_buffer_depth: usize,
    pub ring_depth_samples: Option<usize>,
    pub reason: &'static str,
    pub callback_interval_us: Option<u64>,
    pub callback_budget_us: Option<u64>,
}
/// Fixed-size observations; serialization and context resolution happen in the worker.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Observation {
    pub record_type: &'static str,
    pub rx_monotonic_us: u64,
    pub stream_id: u64,
    pub packet_sequence: Option<u32>,
    pub source_sample_index: Option<u32>,
    pub playout_epoch: u64,
    pub output_epoch: Option<u64>,
    pub output_epoch_end: Option<u64>,
    pub scheduled_us: Option<u64>,
    pub overdue_us: Option<u64>,
    pub accepted_samples: Option<u64>,
    pub dropped_samples: Option<u64>,
    pub discarded_samples: Option<u64>,
    pub resampler_phase: Option<f64>,
    pub resampler_step: Option<f64>,
    pub expected_sequence: Option<u32>,
    pub interval_us: Option<u64>,
    pub sequence_gaps: Option<u64>,
    pub pending_sequence_holes: Option<u64>,
    pub out_of_order: Option<bool>,
    pub duplicate: Option<bool>,
    pub occupancy: Option<usize>,
    pub occupancy_before: Option<usize>,
    pub contiguous_depth: Option<usize>,
    pub capacity: Option<usize>,
    pub late_packet_count: Option<u64>,
    pub underrun_count: Option<u64>,
    pub admitted: Option<bool>,
    pub delivered: Option<bool>,
    pub started: Option<bool>,
    pub operational: Option<bool>,
    pub requested_frames: Option<u64>,
    /// Device-rate frames with no missing resampler input or inserted output.
    pub supplied_frames: Option<u64>,
    pub inserted_frames: Option<u64>,
    pub starvation_frames: Option<u64>,
    pub prefill_frames: Option<u64>,
    pub inactive_frames: Option<u64>,
    pub missing_input_samples: Option<u64>,
    pub consumed_input_samples: Option<u64>,
    pub output_rate: Option<u32>,
    pub budget_us: Option<u64>,
    pub ring_before: Option<usize>,
    pub ring_after: Option<usize>,
    pub reason: Option<&'static str>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub event: Event,
    pub clock_id: String,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
}
#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub now_monotonic_us: u64,
    pub clock_id: String,
    pub current_stream_id: u64,
    pub current_device_id: Option<String>,
    pub current_session_id: Option<String>,
    pub enabled: bool,
    pub enabled_changes: u64,
    pub counts: [u64; 13],
    pub counts_60s: [u64; 13],
    pub event_types: Vec<EventType>,
    pub drop_count: u64,
    pub observation_drop_count: u64,
    pub recent: Vec<Record>,
    pub event_log_path: Option<String>,
    pub event_log_error: Option<String>,
    pub drift_manifest_path: Option<String>,
    pub drift_summary_path: Option<String>,
    pub drift_telemetry: bool,
}
#[derive(Default)]
struct State {
    recent: VecDeque<Record>,
    buckets: BTreeMap<u64, [u64; 13]>,
    path: Option<String>,
    error: Option<String>,
    manifest: Option<String>,
    summary: Option<String>,
}
static CLOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub struct Diagnostics {
    pub(crate) drift_enabled: bool,
    pub(crate) playout_epoch: AtomicU64,
    origin: Instant,
    clock_id: String,
    pub(crate) queue: ArrayQueue<Event>,
    observations: ArrayQueue<Observation>,
    observation_drops: AtomicU64,
    enabled: AtomicBool,
    enabled_changes: AtomicU64,
    drops: AtomicU64,
    counts: [AtomicU64; 13],
    pub(crate) stream: AtomicU64,
    contexts: Mutex<BTreeMap<u64, (String, String)>>,
    state: Mutex<State>,
    pub(crate) depth: AtomicU64,
    pub(crate) target: AtomicU64,
}
impl Diagnostics {
    pub fn new() -> Arc<Self> {
        let d = Self::bounded(2048);
        d.set_enabled(std::env::var("MIC_DISABLE_DIAGNOSTICS").as_deref() != Ok("1"));
        let weak = Arc::downgrade(&d);
        let clock = d.clock_id.clone();
        std::thread::spawn(move || {
            let mut sink = crate::drift::Sink::new(diagnostics_dir(), clock);
            let mut summaries = crate::drift::Summaries::default();
            let mut last_health = 0;
            let mut last_manifest = 0;
            while let Some(d) = weak.upgrade() {
                let mut records: Vec<serde_json::Value> = d
                    .drain()
                    .iter()
                    .map(|r| serde_json::to_value(r).unwrap())
                    .collect();
                records.extend(d.drain_observations());
                let now = d.now_us();
                if now.saturating_sub(last_health) >= 100_000 {
                    last_health = now;
                    let snapshot = d.snapshot();
                    records.push(serde_json::json!({
                        "record_type": "diagnostic_health", "rx_monotonic_us": now,
                        "clock_id": snapshot.clock_id, "stream_id": snapshot.current_stream_id,
                        "session_id": snapshot.current_session_id, "device_id": snapshot.current_device_id,
                        "enabled": snapshot.enabled, "event_drops": snapshot.drop_count,
                        "enabled_changes": snapshot.enabled_changes,
                        "observation_drops": snapshot.observation_drop_count,
                        "event_counts": snapshot.counts, "event_types": snapshot.event_types,
                        "log_error": snapshot.event_log_error,
                    }));
                }
                for record in &records {
                    if d.drift_enabled {
                        summaries.add(record);
                    }
                    if let Err(e) = sink.detail(record) {
                        d.state.lock().unwrap().error = Some(e.to_string());
                    }
                }
                if d.drift_enabled {
                    for summary in
                        summaries.finish_before(now.saturating_sub(2_000_000) / 1_000_000)
                    {
                        if let Err(e) = sink.summary(&summary) {
                            d.state.lock().unwrap().error = Some(e.to_string());
                        }
                    }
                }
                if now.saturating_sub(last_manifest) >= 1_000_000 {
                    sink.checkpoint();
                    last_manifest = now;
                }
                if let Err(e) = sink.manifest() {
                    d.state.lock().unwrap().error = Some(e.to_string());
                }
                {
                    let mut state = d.state.lock().unwrap();
                    state.path = sink.current_path();
                    state.manifest = sink.manifest_path();
                    state.summary = sink.summary_path();
                }
                drop(d);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        d
    }
    pub(crate) fn bounded(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            drift_enabled: std::env::var("MIC_DRIFT_TELEMETRY").as_deref() != Ok("off"),
            playout_epoch: AtomicU64::new(0),
            origin: Instant::now(),
            clock_id: format!(
                "rx-{}-{}-{}",
                std::process::id(),
                wall_ms(),
                CLOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            queue: ArrayQueue::new(capacity),
            observations: ArrayQueue::new(capacity.max(4096)),
            observation_drops: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
            enabled_changes: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            stream: AtomicU64::new(0),
            contexts: Mutex::new(BTreeMap::new()),
            state: Mutex::new(State::default()),
            depth: AtomicU64::new(0),
            target: AtomicU64::new(0),
        })
    }
    pub fn drift_telemetry_enabled(&self) -> bool {
        self.drift_enabled
    }
    pub fn now_us(&self) -> u64 {
        self.origin.elapsed().as_micros() as u64
    }
    pub fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::Relaxed) != enabled {
            self.enabled_changes.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn set_context(&self, device: &str, session: &str) {
        let id = self.stream.load(Ordering::Relaxed) + 1;
        let mut contexts = self.contexts.lock().unwrap();
        contexts.insert(id, (device.into(), session.into()));
        while contexts.len() > 128 {
            contexts.pop_first();
        }
        self.stream.store(id, Ordering::Release);
        drop(contexts);
        self.observe(self.observation("stream_start"));
    }
    pub fn current_stream_id(&self) -> u64 {
        self.stream.load(Ordering::Acquire)
    }
    #[allow(clippy::too_many_arguments)] // Fixed scalar RT payload; no allocation or locking.
    pub fn emit(
        &self,
        event_type: EventType,
        packet: Option<u32>,
        expected: Option<u32>,
        depth: usize,
        target: usize,
        ring: Option<usize>,
        reason: &'static str,
    ) {
        self.emit_deadline(
            event_type, packet, expected, depth, target, ring, reason, None,
        );
    }
    #[allow(clippy::too_many_arguments)]
    pub fn emit_deadline(
        &self,
        event_type: EventType,
        packet: Option<u32>,
        expected: Option<u32>,
        depth: usize,
        target: usize,
        ring: Option<usize>,
        reason: &'static str,
        deadline_sequence: Option<u32>,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.counts[event_type as usize].fetch_add(1, Ordering::Relaxed);
        let event = Event {
            playout_epoch: self.playout_epoch.load(Ordering::Relaxed),
            deadline_sequence,
            event_type,
            rx_monotonic_us: self.now_us(),
            stream_id: self.stream.load(Ordering::Acquire),
            packet_sequence: packet,
            expected_sequence: expected,
            jitter_buffer_depth: depth,
            target_buffer_depth: target,
            ring_depth_samples: ring,
            reason,
            callback_interval_us: None,
            callback_budget_us: None,
        };
        if self.queue.push(event).is_err() {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn callback_delay(&self, interval_us: u64, budget_us: u64, ring: usize) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.counts[EventType::AudioCallbackDelay as usize].fetch_add(1, Ordering::Relaxed);
        let event = Event {
            playout_epoch: self.playout_epoch.load(Ordering::Relaxed),
            deadline_sequence: None,
            event_type: EventType::AudioCallbackDelay,
            rx_monotonic_us: self.now_us(),
            stream_id: self.stream.load(Ordering::Acquire),
            packet_sequence: None,
            expected_sequence: None,
            jitter_buffer_depth: self.depth.load(Ordering::Relaxed) as usize,
            target_buffer_depth: self.target.load(Ordering::Relaxed) as usize,
            ring_depth_samples: Some(ring),
            reason: "callback_entry_gap_over_previous_buffer_duration_plus_20ms",
            callback_interval_us: Some(interval_us),
            callback_budget_us: Some(budget_us),
        };
        if self.queue.push(event).is_err() {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn observe(&self, observation: Observation) {
        if self.enabled.load(Ordering::Relaxed) && self.observations.push(observation).is_err() {
            self.observation_drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn observation(&self, record_type: &'static str) -> Observation {
        Observation {
            record_type,
            rx_monotonic_us: self.now_us(),
            stream_id: self.stream.load(Ordering::Acquire),
            playout_epoch: self.playout_epoch.load(Ordering::Relaxed),
            ..Observation::default()
        }
    }
    #[cfg(test)]
    pub(crate) fn discard_observations(&self) {
        while self.observations.pop().is_some() {}
    }
    pub(crate) fn drain_observations(&self) -> Vec<serde_json::Value> {
        let mut records = Vec::new();
        for _ in 0..self.observations.capacity() {
            let Some(o) = self.observations.pop() else {
                break;
            };
            let context = self.contexts.lock().unwrap().get(&o.stream_id).cloned();
            let mut value = serde_json::to_value(o).unwrap();
            value["clock_id"] = self.clock_id.clone().into();
            value["device_id"] = serde_json::to_value(context.as_ref().map(|c| &c.0)).unwrap();
            value["session_id"] = serde_json::to_value(context.as_ref().map(|c| &c.1)).unwrap();
            records.push(value);
        }
        records
    }
    pub(crate) fn drain(&self) -> Vec<Record> {
        let mut records = Vec::new();
        // Fixed work bound even if producers keep the queue busy.
        for _ in 0..self.queue.capacity() {
            let Some(event) = self.queue.pop() else {
                break;
            };
            let context = self.contexts.lock().unwrap().get(&event.stream_id).cloned();
            records.push(Record {
                event,
                clock_id: self.clock_id.clone(),
                device_id: context.as_ref().map(|c| c.0.clone()),
                session_id: context.map(|c| c.1),
            });
        }
        let mut state = self.state.lock().unwrap();
        for record in &records {
            state
                .buckets
                .entry(record.event.rx_monotonic_us / 1_000_000)
                .or_default()[record.event.event_type as usize] += 1;
            state.recent.push_back(record.clone());
            if state.recent.len() > 128 {
                state.recent.pop_front();
            }
        }
        let cutoff = (self.now_us() / 1_000_000).saturating_sub(59);
        state.buckets.retain(|second, _| *second >= cutoff);
        records
    }
    pub fn snapshot(&self) -> Snapshot {
        let context = self
            .contexts
            .lock()
            .unwrap()
            .get(&self.stream.load(Ordering::Acquire))
            .cloned();
        let state = self.state.lock().unwrap();
        let now = self.now_us();
        let mut counts_60s = [0; 13];
        for (_, counts) in state.buckets.range((now / 1_000_000).saturating_sub(59)..) {
            for i in 0..13 {
                counts_60s[i] += counts[i];
            }
        }
        Snapshot {
            now_monotonic_us: now,
            clock_id: self.clock_id.clone(),
            current_stream_id: self.stream.load(Ordering::Acquire),
            current_device_id: context.as_ref().map(|c| c.0.clone()),
            current_session_id: context.map(|c| c.1),
            enabled: self.enabled.load(Ordering::Relaxed),
            enabled_changes: self.enabled_changes.load(Ordering::Relaxed),
            counts: std::array::from_fn(|i| self.counts[i].load(Ordering::Relaxed)),
            counts_60s,
            event_types: EVENT_TYPES.to_vec(),
            drop_count: self.drops.load(Ordering::Relaxed),
            observation_drop_count: self.observation_drops.load(Ordering::Relaxed),
            recent: state.recent.iter().cloned().collect(),
            event_log_path: state.path.clone(),
            event_log_error: state.error.clone(),
            drift_manifest_path: state.manifest.clone(),
            drift_summary_path: state.summary.clone(),
            drift_telemetry: self.drift_enabled,
        }
    }
}
pub(crate) fn diagnostics_dir() -> PathBuf {
    std::env::var_os("MIC_DIAGNOSTICS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .join("Diagnostics")
        })
}
pub(crate) fn wall_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_overflow_disable_and_session_identity() {
        let d = Diagnostics::bounded(1);
        d.set_context("device", "first");
        for _ in 0..4096 {
            d.observe(d.observation("udp_receive"));
        }
        assert!(d.snapshot().observation_drop_count > 0);
        let rows = d.drain_observations();
        assert_eq!(rows.len(), 4096);
        assert!(rows.iter().all(|r| r["session_id"] == "first"));
        d.set_enabled(false);
        d.observe(d.observation("audio_callback"));
        assert!(d.drain_observations().is_empty());
        d.set_enabled(true);
        d.set_context("device", "second");
        let rows = d.drain_observations();
        assert_eq!(rows[0]["session_id"], "second");
        assert_eq!(rows[0]["stream_id"], 2);
    }
    #[test]
    fn bounded_drop_disabled_and_context() {
        let d = Diagnostics::bounded(1);
        d.set_context("device", "session");
        for _ in 0..3 {
            d.emit(EventType::LatePacket, Some(3), Some(4), 2, 3, None, "test");
        }
        d.drain();
        let s = d.snapshot();
        assert_eq!(s.drop_count, 2);
        assert_eq!(s.counts[1], 3);
        assert_eq!(s.counts_60s[1], 1);
        assert_eq!(s.recent[0].device_id.as_deref(), Some("device"));
        d.set_enabled(false);
        d.emit(EventType::LatePacket, None, None, 0, 3, None, "disabled");
        assert_eq!(d.snapshot().counts[1], 3);
    }
}
