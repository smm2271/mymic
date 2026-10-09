use mic_device::State;
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{RwLock, broadcast, mpsc, watch},
    time::{self, Duration, Instant},
};
pub const CAPACITY: usize = 900;
pub const RETENTION_MS: u64 = 900_000;
pub const EVENT_CAPACITY: usize = 4096;
pub const MAX_DEVICES: usize = 64;
pub fn utc_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Counters {
    pub received_packets: u64,
    pub lost_packets: u64,
    pub out_of_order_packets: u64,
    pub received_bytes: u64,
    pub buffer_underruns: u64,
    pub buffer_overruns: u64,
}
impl Counters {
    pub fn checked_delta(self, previous: Self) -> Option<Self> {
        Some(Self {
            received_packets: self
                .received_packets
                .checked_sub(previous.received_packets)?,
            lost_packets: self.lost_packets.checked_sub(previous.lost_packets)?,
            out_of_order_packets: self
                .out_of_order_packets
                .checked_sub(previous.out_of_order_packets)?,
            received_bytes: self.received_bytes.checked_sub(previous.received_bytes)?,
            buffer_underruns: self
                .buffer_underruns
                .checked_sub(previous.buffer_underruns)?,
            buffer_overruns: self.buffer_overruns.checked_sub(previous.buffer_overruns)?,
        })
    }
    pub fn add(&mut self, delta: Self) {
        self.received_packets = self.received_packets.saturating_add(delta.received_packets);
        self.lost_packets = self.lost_packets.saturating_add(delta.lost_packets);
        self.out_of_order_packets = self
            .out_of_order_packets
            .saturating_add(delta.out_of_order_packets);
        self.received_bytes = self.received_bytes.saturating_add(delta.received_bytes);
        self.buffer_underruns = self.buffer_underruns.saturating_add(delta.buffer_underruns);
        self.buffer_overruns = self.buffer_overruns.saturating_add(delta.buffer_overruns);
    }
}
#[derive(Clone)]
pub struct DeviceInput {
    pub device_id: String,
    pub session_id: Option<String>,
    pub connection_state: State,
    pub counters: Counters,
    pub jitter_buffer_depth: usize,
    pub activity_epoch: u64,
    pub counter_timestamp_ms: Option<u64>,
    pub tx: Option<mic_protocol::diagnostics::TxTiming>,
}
#[derive(Clone)]
pub struct Inputs {
    pub devices: Vec<DeviceInput>,
    pub updated: Instant,
}
impl Default for Inputs {
    fn default() -> Self {
        Self {
            devices: vec![],
            updated: Instant::now(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct TelemetrySnapshot {
    pub id: String,
    pub sequence: u64,
    pub timestamp: u64,
    pub device_id: String,
    pub session_id: Option<String>,
    pub connection_state: State,
    pub streaming_state: bool,
    pub interval_ms: Option<u64>,
    pub received_packets_delta: Option<u64>,
    pub lost_packets_delta: Option<u64>,
    pub out_of_order_packets_delta: Option<u64>,
    pub receive_bitrate: Option<f64>,
    pub packet_loss_rate: Option<f64>,
    pub jitter_buffer_depth: Option<usize>,
    pub buffer_underruns_delta: Option<u64>,
    pub buffer_overruns_delta: Option<u64>,
    pub buffer_underruns_per_second: Option<f64>,
    pub cumulative: Counters,
    pub discontinuity: Option<String>,
    pub loss_basis: &'static str,
    pub bitrate_basis: &'static str,
    pub rx_packet_rate: Option<f64>,
    pub tx_packet_rate: Option<f64>,
    pub tx_scheduling_drift_ms: Option<f64>,
    pub tx: Option<mic_protocol::diagnostics::TxTiming>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    StartStream,
    StopStream,
    Disconnect,
    Reconnect,
    SessionChanged,
}
#[derive(Clone)]
pub struct EventInput {
    pub timestamp: u64,
    pub device_id: String,
    pub session_id: Option<String>,
    pub event_type: EventType,
}
#[derive(Clone, Debug, Serialize)]
pub struct TelemetryEvent {
    pub id: String,
    pub sequence: u64,
    pub timestamp: u64,
    pub device_id: String,
    pub session_id: Option<String>,
    pub event_type: EventType,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notification {
    TelemetrySnapshot {
        stream_id: String,
        snapshot: Box<TelemetrySnapshot>,
    },
    TelemetryEvent {
        stream_id: String,
        event: TelemetryEvent,
    },
}
#[derive(Clone, Serialize)]
pub struct History {
    pub stream_id: String,
    pub device_id: String,
    pub range: String,
    pub generated_at: u64,
    pub snapshots: Vec<TelemetrySnapshot>,
    pub events: Vec<TelemetryEvent>,
    pub packet_loss_rate_60s: Option<f64>,
    pub dropped_event_inputs: u64,
}
struct Previous {
    input: DeviceInput,
    monotonic_ms: u64,
}
struct DeviceHistory {
    snapshots: VecDeque<TelemetrySnapshot>,
    events: VecDeque<TelemetryEvent>,
    previous: Option<Previous>,
}
impl Default for DeviceHistory {
    fn default() -> Self {
        Self {
            snapshots: VecDeque::with_capacity(CAPACITY),
            events: VecDeque::with_capacity(EVENT_CAPACITY),
            previous: None,
        }
    }
}
impl DeviceHistory {
    fn prune(&mut self, now: u64) {
        let cutoff = now.saturating_sub(RETENTION_MS);
        while self.snapshots.front().is_some_and(|s| s.timestamp < cutoff) {
            self.snapshots.pop_front();
        }
        while self.events.front().is_some_and(|e| e.timestamp < cutoff) {
            self.events.pop_front();
        }
    }
}
pub struct Store {
    stream_id: String,
    sequence: u64,
    devices: HashMap<String, DeviceHistory>,
}
impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}
impl Store {
    pub fn new() -> Self {
        Self {
            stream_id: hex::encode(rand::random::<[u8; 16]>()),
            sequence: 0,
            devices: HashMap::new(),
        }
    }
    fn next(&mut self) -> (u64, String) {
        self.sequence += 1;
        (
            self.sequence,
            format!("{}:{}", self.stream_id, self.sequence),
        )
    }
    pub fn sample(
        &mut self,
        inputs: &Inputs,
        now: u64,
        monotonic_ms: u64,
        fresh: bool,
    ) -> Vec<Notification> {
        let mut notifications = Vec::with_capacity(inputs.devices.len().min(MAX_DEVICES));
        for input in inputs.devices.iter().take(MAX_DEVICES) {
            if !self.devices.contains_key(&input.device_id) && self.devices.len() >= MAX_DEVICES {
                continue;
            }
            let (sequence, id) = self.next();
            let history = self.devices.entry(input.device_id.clone()).or_default();
            history.prune(now);
            let streaming = input.connection_state == State::Streaming;
            let mut discontinuity = None;
            let mut interval = None;
            let mut delta = None;
            if !streaming {
                discontinuity = Some("not_streaming".into());
            } else if !fresh {
                discontinuity = Some("stale_input".into());
            } else if let Some(previous) = &history.previous {
                let elapsed = input
                    .counter_timestamp_ms
                    .unwrap_or(monotonic_ms)
                    .checked_sub(
                        previous
                            .input
                            .counter_timestamp_ms
                            .unwrap_or(previous.monotonic_ms),
                    );
                if input.session_id != previous.input.session_id {
                    discontinuity = Some("session_changed".into());
                } else if previous.input.connection_state != State::Streaming
                    || input.activity_epoch != previous.input.activity_epoch
                {
                    discontinuity = Some("stream_transition".into());
                } else if !(500..=1500)
                    .contains(&monotonic_ms.saturating_sub(previous.monotonic_ms))
                    || !elapsed.is_some_and(|ms| (500..=1500).contains(&ms))
                {
                    discontinuity = Some("sampling_gap".into());
                } else if let Some(d) = input.counters.checked_delta(previous.input.counters) {
                    interval = elapsed;
                    delta = Some(d);
                } else {
                    discontinuity = Some("counter_reset".into());
                }
            } else {
                discontinuity = Some("baseline".into());
            }
            let seconds = interval.map(|ms| ms as f64 / 1000.0);
            let packet_loss_rate = delta.and_then(|d| {
                let total = d.received_packets.saturating_add(d.lost_packets);
                (total > 0).then(|| d.lost_packets as f64 / total as f64 * 100.0)
            });
            let sample = TelemetrySnapshot {
                id,
                sequence,
                timestamp: now,
                device_id: input.device_id.clone(),
                session_id: input.session_id.clone(),
                connection_state: input.connection_state,
                streaming_state: streaming,
                interval_ms: interval,
                received_packets_delta: delta.map(|d| d.received_packets),
                lost_packets_delta: delta.map(|d| d.lost_packets),
                out_of_order_packets_delta: delta.map(|d| d.out_of_order_packets),
                receive_bitrate: delta
                    .zip(seconds)
                    .map(|(d, s)| d.received_bytes as f64 * 8.0 / s),
                packet_loss_rate,
                jitter_buffer_depth: (streaming && fresh).then_some(input.jitter_buffer_depth),
                buffer_underruns_delta: delta.map(|d| d.buffer_underruns),
                buffer_overruns_delta: delta.map(|d| d.buffer_overruns),
                buffer_underruns_per_second: delta
                    .zip(seconds)
                    .map(|(d, s)| d.buffer_underruns as f64 / s),
                cumulative: input.counters,
                loss_basis: "playout_deadline_deficit",
                bitrate_basis: "pcm_payload",
                rx_packet_rate: delta
                    .zip(seconds)
                    .map(|(d, s)| d.received_packets as f64 / s),
                tx_packet_rate: streaming
                    .then(|| input.tx.as_ref().map(|t| t.packets_per_second))
                    .flatten(),
                tx_scheduling_drift_ms: streaming
                    .then(|| input.tx.as_ref().map(|t| t.scheduling_drift_ms))
                    .flatten(),
                tx: input.tx.clone(),
                discontinuity,
            };
            if history.snapshots.len() == CAPACITY {
                history.snapshots.pop_front();
            }
            history.snapshots.push_back(sample.clone());
            // A stale input cannot become the next interval's baseline.
            history.previous = if fresh {
                Some(Previous {
                    input: input.clone(),
                    monotonic_ms,
                })
            } else {
                None
            };
            notifications.push(Notification::TelemetrySnapshot {
                stream_id: self.stream_id.clone(),
                snapshot: Box::new(sample),
            });
        }
        for history in self.devices.values_mut() {
            history.prune(now);
        }
        notifications
    }
    pub fn event(&mut self, input: EventInput) -> Option<Notification> {
        if !self.devices.contains_key(&input.device_id) && self.devices.len() >= MAX_DEVICES {
            return None;
        }
        let (sequence, id) = self.next();
        let event = TelemetryEvent {
            id,
            sequence,
            timestamp: input.timestamp,
            device_id: input.device_id.clone(),
            session_id: input.session_id,
            event_type: input.event_type,
        };
        let h = self.devices.entry(input.device_id).or_default();
        h.prune(input.timestamp);
        if h.events.len() == EVENT_CAPACITY {
            h.events.pop_front();
        }
        h.events.push_back(event.clone());
        Some(Notification::TelemetryEvent {
            stream_id: self.stream_id.clone(),
            event,
        })
    }
    pub fn history(&self, id: &str, range: Range, now: u64) -> Option<History> {
        let h = self.devices.get(id)?;
        let cutoff = now.saturating_sub(range.milliseconds());
        let min60 = now.saturating_sub(60_000);
        let mut received = 0u64;
        let mut lost = 0u64;
        let mut valid = false;
        for sample in h
            .snapshots
            .iter()
            .filter(|s| s.timestamp > min60 && s.timestamp <= now)
        {
            if let (Some(r), Some(l)) = (sample.received_packets_delta, sample.lost_packets_delta) {
                received = received.saturating_add(r);
                lost = lost.saturating_add(l);
                valid = true;
            }
        }
        let total = received.saturating_add(lost);
        let rate = (valid && total > 0).then(|| lost as f64 / total as f64 * 100.0);
        let mut snapshots: Vec<_> = h
            .snapshots
            .iter()
            .filter(|s| s.timestamp >= cutoff && s.timestamp <= now)
            .cloned()
            .collect();
        snapshots.sort_by_key(|s| (s.timestamp, s.sequence));
        let mut events: Vec<_> = h
            .events
            .iter()
            .filter(|s| s.timestamp >= cutoff && s.timestamp <= now)
            .cloned()
            .collect();
        events.sort_by_key(|e| (e.timestamp, e.sequence));
        Some(History {
            stream_id: self.stream_id.clone(),
            device_id: id.into(),
            range: range.as_str().into(),
            generated_at: now,
            snapshots,
            events,
            packet_loss_rate_60s: rate,
            dropped_event_inputs: 0,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Range {
    Seconds60,
    Minutes5,
    Minutes15,
}
impl Range {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "60s" => Some(Self::Seconds60),
            "5m" => Some(Self::Minutes5),
            "15m" => Some(Self::Minutes15),
            _ => None,
        }
    }
    pub fn milliseconds(self) -> u64 {
        match self {
            Self::Seconds60 => 60_000,
            Self::Minutes5 => 300_000,
            Self::Minutes15 => 900_000,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seconds60 => "60s",
            Self::Minutes5 => "5m",
            Self::Minutes15 => "15m",
        }
    }
}
pub fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
#[derive(Clone)]
pub struct Telemetry {
    pub store: Arc<RwLock<Store>>,
    pub notifications: broadcast::Sender<Notification>,
    inputs: watch::Sender<Inputs>,
    events: mpsc::Sender<EventInput>,
    dropped: Arc<AtomicU64>,
}
impl Telemetry {
    pub fn start() -> (Self, tokio::task::JoinHandle<()>) {
        let (inputs, input_rx) = watch::channel(Inputs::default());
        let (events, event_rx) = mpsc::channel(1024);
        let (notifications, _) = broadcast::channel(128);
        let service = Self {
            store: Arc::new(RwLock::new(Store::new())),
            notifications,
            inputs,
            events,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let task = tokio::spawn(sample_loop(
            service.store.clone(),
            service.notifications.clone(),
            input_rx,
            event_rx,
        ));
        (service, task)
    }
    pub fn publish(&self, devices: Vec<DeviceInput>) {
        self.inputs.send_replace(Inputs {
            devices,
            updated: Instant::now(),
        });
    }
    pub fn event(&self, id: &str, session: Option<String>, kind: EventType) {
        if self
            .events
            .try_send(EventInput {
                timestamp: utc_ms(),
                device_id: id.into(),
                session_id: session,
                event_type: kind,
            })
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub async fn history(&self, id: &str, range: Range) -> Option<History> {
        let store = self.store.read().await;
        let now = utc_ms();
        let mut history = store.history(id, range, now).or_else(|| {
            self.inputs
                .borrow()
                .devices
                .iter()
                .any(|d| d.device_id == id)
                .then(|| History {
                    stream_id: store.stream_id.clone(),
                    device_id: id.into(),
                    range: range.as_str().into(),
                    generated_at: now,
                    snapshots: vec![],
                    events: vec![],
                    packet_loss_rate_60s: None,
                    dropped_event_inputs: 0,
                })
        })?;
        history.dropped_event_inputs = self.dropped.load(Ordering::Relaxed);
        Some(history)
    }
}
async fn sample_loop(
    store: Arc<RwLock<Store>>,
    notifications: broadcast::Sender<Notification>,
    mut inputs: watch::Receiver<Inputs>,
    mut events: mpsc::Receiver<EventInput>,
) {
    let started = Instant::now();
    let mut timer = time::interval_at(started + Duration::from_secs(1), Duration::from_secs(1));
    timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
         changed=inputs.changed()=>{if changed.is_err(){break}},
         event=events.recv()=>{if let Some(event)=event {let note=store.write().await.event(event);if let Some(note)=note {let _=notifications.send(note);}}else{break}},
         _=timer.tick()=>{let current=inputs.borrow().clone();let fresh=current.updated.elapsed()<=Duration::from_millis(1500);let notes=store.write().await.sample(&current,utc_ms(),started.elapsed().as_millis() as u64,fresh);for note in notes{let _=notifications.send(note);}}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(id: &str, session: &str, n: u64) -> DeviceInput {
        DeviceInput {
            device_id: id.into(),
            session_id: Some(session.into()),
            connection_state: State::Streaming,
            counters: Counters {
                received_packets: n,
                lost_packets: n / 10,
                out_of_order_packets: n / 20,
                received_bytes: n * 368,
                buffer_underruns: n / 10,
                buffer_overruns: n / 100,
            },
            jitter_buffer_depth: 3,
            activity_epoch: 0,
            counter_timestamp_ms: None,
            tx: None,
        }
    }
    fn feed(store: &mut Store, d: DeviceInput, t: u64) {
        store.sample(
            &Inputs {
                devices: vec![d],
                updated: Instant::now(),
            },
            t,
            t,
            true,
        );
    }
    #[test]
    fn adjacent_deltas_and_rates() {
        let mut s = Store::new();
        feed(&mut s, input("a", "one", 100), 1000);
        feed(&mut s, input("a", "one", 200), 2000);
        let h = s.history("a", Range::Seconds60, 2000).unwrap();
        let p = &h.snapshots[1];
        assert_eq!(p.received_packets_delta, Some(100));
        assert_eq!(p.receive_bitrate, Some(294400.0));
        assert_eq!(p.buffer_underruns_delta, Some(10));
        assert!((p.packet_loss_rate.unwrap() - 100.0 / 11.0).abs() < 0.001);
        assert_eq!(h.snapshots[0].received_packets_delta, None);
    }
    #[test]
    fn ring_capacity_and_age_expiration() {
        let mut s = Store::new();
        for n in 0..1000 {
            feed(&mut s, input("a", "one", n), n * 1000)
        }
        let h = s.history("a", Range::Minutes15, 999000).unwrap();
        assert_eq!(h.snapshots.len(), 900);
        assert_eq!(h.snapshots[0].timestamp, 100000);
        s.sample(&Inputs::default(), 2_000_000, 2_000_000, true);
        assert!(
            s.history("a", Range::Minutes15, 2_000_000)
                .unwrap()
                .snapshots
                .is_empty()
        );
    }
    #[test]
    fn session_reset_and_sampling_gap_are_null() {
        let mut s = Store::new();
        feed(&mut s, input("a", "one", 100), 1000);
        feed(&mut s, input("a", "two", 1), 2000);
        feed(&mut s, input("a", "two", 0), 3000);
        feed(&mut s, input("a", "two", 2000), 7000);
        let h = s.history("a", Range::Seconds60, 7000).unwrap();
        for sample in &h.snapshots {
            assert!(sample.packet_loss_rate.is_none());
            assert!(sample.received_packets_delta.is_none());
        }
        assert_eq!(
            h.snapshots[1].discontinuity.as_deref(),
            Some("session_changed")
        );
        assert_eq!(
            h.snapshots[2].discontinuity.as_deref(),
            Some("counter_reset")
        );
        assert_eq!(
            h.snapshots[3].discontinuity.as_deref(),
            Some("sampling_gap")
        );
    }
    #[test]
    fn stop_and_between_sample_restart_dont_fake_underruns() {
        let mut s = Store::new();
        feed(&mut s, input("a", "one", 100), 1000);
        let mut d = input("a", "one", 100);
        d.connection_state = State::Connected;
        feed(&mut s, d, 2000);
        let mut d = input("a", "one", 300);
        d.activity_epoch = 2;
        feed(&mut s, d.clone(), 3000);
        d.activity_epoch = 4;
        feed(&mut s, d, 4000);
        let h = s.history("a", Range::Seconds60, 4000).unwrap();
        for p in h.snapshots {
            assert!(p.buffer_underruns_delta.is_none());
            assert!(p.receive_bitrate.is_none());
        }
    }
    #[test]
    fn range_filter_and_device_isolation() {
        let mut s = Store::new();
        for n in 0..900 {
            let devices = vec![input("a", "one", n * 100), input("b", "two", n * 2)];
            s.sample(
                &Inputs {
                    devices,
                    updated: Instant::now(),
                },
                n * 1000,
                n * 1000,
                true,
            );
        }
        for (range, count) in [
            (Range::Seconds60, 61),
            (Range::Minutes5, 301),
            (Range::Minutes15, 900),
        ] {
            let h = s.history("a", range, 899000).unwrap();
            assert_eq!(h.snapshots.len(), count);
            assert!(h.snapshots.iter().all(|p| p.device_id == "a"));
        }
        let a = s.history("a", Range::Seconds60, 899000).unwrap();
        let b = s.history("b", Range::Seconds60, 899000).unwrap();
        assert_eq!(
            a.snapshots.last().unwrap().received_packets_delta,
            Some(100)
        );
        assert_eq!(b.snapshots.last().unwrap().received_packets_delta, Some(2));
    }
    #[test]
    fn event_capacity_and_retention() {
        let mut s = Store::new();
        for n in 0..EVENT_CAPACITY + 5 {
            s.event(EventInput {
                timestamp: n as u64,
                device_id: "a".into(),
                session_id: None,
                event_type: EventType::StartStream,
            });
        }
        assert_eq!(
            s.history("a", Range::Minutes15, 10000)
                .unwrap()
                .events
                .len(),
            EVENT_CAPACITY
        );
        s.sample(
            &Inputs::default(),
            RETENTION_MS + 10000,
            RETENTION_MS + 10000,
            true,
        );
        assert!(
            s.history("a", Range::Minutes15, RETENTION_MS + 10000)
                .unwrap()
                .events
                .is_empty()
        );
    }
    #[test]
    fn weighted_60s_rate_excludes_old_samples() {
        let mut s = Store::new();
        for n in 0..70 {
            let mut d = input("a", "one", n * 100);
            d.counters.lost_packets = if n < 5 { n * 1000 } else { 4000 };
            feed(&mut s, d, n * 1000);
        }
        assert_eq!(
            s.history("a", Range::Minutes15, 69000)
                .unwrap()
                .packet_loss_rate_60s,
            Some(0.0)
        );
    }
    #[test]
    fn query_validation() {
        for id in ["", "../x", "a b", "a/b", "é"] {
            assert!(!valid_device_id(id));
        }
        assert!(valid_device_id("esp32-ab_01.test"));
        assert!(Range::parse("1m").is_none());
        assert!(Range::parse("15m").is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn independent_one_second_sampling_and_slow_subscriber() {
        let (service, task) = Telemetry::start();
        service.publish(vec![input("a", "one", 0)]);
        let mut slow = service.notifications.subscribe();
        tokio::task::yield_now().await;
        assert!(
            service
                .history("a", Range::Seconds60)
                .await
                .unwrap()
                .snapshots
                .is_empty()
        );
        for n in 1..=140 {
            service.publish(vec![input("a", "one", n * 100)]);
            time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
        let h = service.store.read().await;
        assert_eq!(h.devices["a"].snapshots.len(), 140);
        drop(h);
        assert!(matches!(
            slow.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        task.abort();
        let _ = task.await;
    }
}

#[cfg(test)]
mod acquisition_tests {
    use super::*;
    #[test]
    fn rates_use_counter_acquisition_interval_not_later_sampler_wakeup() {
        let mut store = Store::new();
        let mut input = DeviceInput {
            device_id: "sim".into(),
            session_id: Some("a".into()),
            connection_state: State::Streaming,
            counters: Counters::default(),
            jitter_buffer_depth: 3,
            activity_epoch: 0,
            counter_timestamp_ms: Some(0),
            tx: None,
        };
        store.sample(
            &Inputs {
                devices: vec![input.clone()],
                updated: Instant::now(),
            },
            0,
            0,
            true,
        );
        input.counter_timestamp_ms = Some(750);
        input.counters.received_packets = 75;
        input.counters.received_bytes = 75 * 320;
        store.sample(
            &Inputs {
                devices: vec![input],
                updated: Instant::now(),
            },
            1000,
            1000,
            true,
        );
        let h = store.history("sim", Range::Seconds60, 1000).unwrap();
        let s = h.snapshots.last().unwrap();
        assert_eq!(s.rx_packet_rate, Some(100.0));
        assert_eq!(s.receive_bitrate, Some(256000.0));
        assert!(s.tx_packet_rate.is_none());
    }
}
