//! Bounded, nonblocking producers; disk work belongs exclusively to the worker.
use crossbeam_queue::ArrayQueue;
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::Write,
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
}
pub const EVENT_TYPES: [EventType; 7] = [
    EventType::BufferUnderrun,
    EventType::LatePacket,
    EventType::BufferResync,
    EventType::AudioRecovery,
    EventType::AudioCallbackStarvation,
    EventType::AudioCallbackDelay,
    EventType::AudioOutputError,
];
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Event {
    pub event_type: EventType,
    pub rx_monotonic_us: u64,
    pub stream_id: u64,
    pub packet_sequence: Option<u32>,
    pub expected_sequence: Option<u32>,
    pub jitter_buffer_depth: usize,
    pub target_buffer_depth: usize,
    pub ring_depth_samples: usize,
    pub reason: &'static str,
    pub callback_interval_us: Option<u64>,
    pub callback_budget_us: Option<u64>,
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
    pub counts: [u64; 7],
    pub counts_60s: [u64; 7],
    pub event_types: Vec<EventType>,
    pub drop_count: u64,
    pub recent: Vec<Record>,
    pub event_log_path: Option<String>,
    pub event_log_error: Option<String>,
}
#[derive(Default)]
struct State {
    recent: VecDeque<Record>,
    buckets: BTreeMap<u64, [u64; 7]>,
    path: Option<String>,
    error: Option<String>,
}
static CLOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub struct Diagnostics {
    origin: Instant,
    clock_id: String,
    pub(crate) queue: ArrayQueue<Event>,
    enabled: AtomicBool,
    drops: AtomicU64,
    counts: [AtomicU64; 7],
    pub(crate) stream: AtomicU64,
    contexts: Mutex<BTreeMap<u64, (String, String)>>,
    state: Mutex<State>,
    pub(crate) depth: AtomicU64,
    pub(crate) target: AtomicU64,
}
impl Diagnostics {
    pub fn new() -> Arc<Self> {
        let d = Self::bounded(2048);
        let weak = Arc::downgrade(&d);
        std::thread::spawn(move || {
            let mut log: Option<File> = None;
            let mut bytes = 0usize;
            while let Some(d) = weak.upgrade() {
                let records = d.drain();
                for record in records {
                    let result = (|| -> std::io::Result<()> {
                        if bytes >= 8 * 1024 * 1024 {
                            return Err(std::io::Error::other(
                                "event log reached 8 MiB limit; memory diagnostics continue",
                            ));
                        }
                        if log.is_none() && d.state.lock().unwrap().error.is_none() {
                            let dir = diagnostics_dir();
                            std::fs::create_dir_all(&dir)?;
                            let path = dir.join(format!(
                                "events-{}-{}.jsonl",
                                wall_ms(),
                                std::process::id()
                            ));
                            log = Some(
                                OpenOptions::new()
                                    .create_new(true)
                                    .write(true)
                                    .open(&path)?,
                            );
                            d.state.lock().unwrap().path = Some(path.display().to_string());
                        }
                        if let Some(file) = &mut log {
                            let line = serde_json::to_vec(&record)?;
                            file.write_all(&line)?;
                            file.write_all(b"\n")?;
                            bytes += line.len() + 1;
                        }
                        Ok(())
                    })();
                    if let Err(e) = result {
                        d.state.lock().unwrap().error = Some(e.to_string());
                        log = None;
                    }
                }
                drop(d);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        d
    }
    pub(crate) fn bounded(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            clock_id: format!(
                "rx-{}-{}-{}",
                std::process::id(),
                wall_ms(),
                CLOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            queue: ArrayQueue::new(capacity),
            enabled: AtomicBool::new(true),
            drops: AtomicU64::new(0),
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            stream: AtomicU64::new(0),
            contexts: Mutex::new(BTreeMap::new()),
            state: Mutex::new(State::default()),
            depth: AtomicU64::new(0),
            target: AtomicU64::new(0),
        })
    }
    pub fn now_us(&self) -> u64 {
        self.origin.elapsed().as_micros() as u64
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn set_context(&self, device: &str, session: &str) {
        let id = self.stream.load(Ordering::Relaxed) + 1;
        let mut contexts = self.contexts.lock().unwrap();
        contexts.insert(id, (device.into(), session.into()));
        while contexts.len() > 128 {
            contexts.pop_first();
        }
        self.stream.store(id, Ordering::Release);
    }
    #[allow(clippy::too_many_arguments)] // Fixed scalar RT payload; no allocation or locking.
    pub fn emit(
        &self,
        event_type: EventType,
        packet: Option<u32>,
        expected: Option<u32>,
        depth: usize,
        target: usize,
        ring: usize,
        reason: &'static str,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.counts[event_type as usize].fetch_add(1, Ordering::Relaxed);
        let event = Event {
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
            event_type: EventType::AudioCallbackDelay,
            rx_monotonic_us: self.now_us(),
            stream_id: self.stream.load(Ordering::Acquire),
            packet_sequence: None,
            expected_sequence: None,
            jitter_buffer_depth: self.depth.load(Ordering::Relaxed) as usize,
            target_buffer_depth: self.target.load(Ordering::Relaxed) as usize,
            ring_depth_samples: ring,
            reason: "callback_entry_gap_over_previous_buffer_duration_plus_20ms",
            callback_interval_us: Some(interval_us),
            callback_budget_us: Some(budget_us),
        };
        if self.queue.push(event).is_err() {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
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
        let mut counts_60s = [0; 7];
        for (_, counts) in state.buckets.range((now / 1_000_000).saturating_sub(59)..) {
            for i in 0..7 {
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
            counts: std::array::from_fn(|i| self.counts[i].load(Ordering::Relaxed)),
            counts_60s,
            event_types: EVENT_TYPES.to_vec(),
            drop_count: self.drops.load(Ordering::Relaxed),
            recent: state.recent.iter().cloned().collect(),
            event_log_path: state.path.clone(),
            event_log_error: state.error.clone(),
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
    fn bounded_drop_disabled_and_context() {
        let d = Diagnostics::bounded(1);
        d.set_context("device", "session");
        for _ in 0..3 {
            d.emit(EventType::LatePacket, Some(3), Some(4), 2, 3, 0, "test");
        }
        d.drain();
        let s = d.snapshot();
        assert_eq!(s.drop_count, 2);
        assert_eq!(s.counts[1], 3);
        assert_eq!(s.counts_60s[1], 1);
        assert_eq!(s.recent[0].device_id.as_deref(), Some("device"));
        d.set_enabled(false);
        d.emit(EventType::LatePacket, None, None, 0, 3, 0, "disabled");
        assert_eq!(d.snapshot().counts[1], 3);
    }
}
