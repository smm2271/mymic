use crate::timing::{Counts, DeadlineClock, Mode, PERIOD, Report, Waiter, distribution};
use anyhow::Result;
use mic_protocol::{AudioPacket, Generator, Source};
use std::{
    collections::VecDeque,
    io::{BufWriter, Write},
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Settings {
    pub running: bool,
    pub epoch: u64,
    pub session: [u8; 16],
    pub key: [u8; 32],
    pub peer: Option<SocketAddr>,
    pub diagnostic_peer: Option<SocketAddr>,
    pub source: Source,
    pub gain: f32,
    pub mute: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            running: false,
            epoch: 0,
            session: [0; 16],
            key: [0; 32],
            peer: None,
            diagnostic_peer: None,
            source: Source::Sine,
            gain: 1.0,
            mute: false,
        }
    }
}
pub struct Transmitter {
    pub settings: Arc<Mutex<Settings>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
}
#[derive(Clone)]
pub struct Options {
    pub mode: Mode,
    pub device_id: String,
    pub loss: u32,
    pub reorder: u32,
    pub jitter_ms: u64,
    pub diagnostic_path: Option<String>,
}
impl Transmitter {
    pub fn start(options: Options) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_nonblocking(true)?;
        // Fail startup visibly if the selected Windows timing primitive is unavailable.
        if options.mode == Mode::Waitable {
            drop(Waiter::new()?);
        }
        let settings = Arc::new(Mutex::new(Settings::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_settings = settings.clone();
        let worker_stop = stop.clone();
        if let Some(path) = &options.diagnostic_path
            && let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let diagnostic_file = options
            .diagnostic_path
            .as_ref()
            .map(std::fs::File::create)
            .transpose()?;
        let worker = std::thread::spawn(move || {
            run(
                socket,
                worker_settings,
                worker_stop,
                options,
                diagnostic_file,
            )
        });
        Ok(Self {
            settings,
            stop,
            worker: Some(worker),
        })
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.settings.lock().unwrap().running = false;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("TX timing thread panicked"))??;
        }
        Ok(())
    }
}
impl Drop for Transmitter {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
fn run(
    socket: UdpSocket,
    settings: Arc<Mutex<Settings>>,
    stop: Arc<AtomicBool>,
    options: Options,
    file: Option<std::fs::File>,
) -> Result<()> {
    let multimedia_thread =
        (options.mode == Mode::Waitable).then(crate::timing::MultimediaThread::register);
    let mmcss_active = multimedia_thread.as_ref().is_some_and(|t| t.active);
    let _period = matches!(options.mode, Mode::Period | Mode::Current)
        .then(mic_protocol::TimerResolution::acquire);
    let waiter = if options.mode == Mode::Waitable {
        Some(Waiter::new()?)
    } else {
        None
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let mut ticker = runtime.block_on(async {
        let mut t = tokio::time::interval(PERIOD);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        t
    });
    let (log_tx, log_rx) = mpsc::sync_channel::<Report>(16);
    let diagnostic_settings = settings.clone();
    let device_id = options.device_id.clone();
    let logger = std::thread::spawn(move || -> Result<()> {
        let mut file = file.map(BufWriter::new);
        let diagnostic_socket = UdpSocket::bind("0.0.0.0:0")?;
        diagnostic_socket.set_nonblocking(true)?;
        let mut sequence = 0;

        while let Ok(report) = log_rx.recv() {
            sequence += 1;
            let state = diagnostic_settings.lock().unwrap().clone();
            if let Some(peer) = state.diagnostic_peer
                && hex::encode(state.session) == report.session_id
            {
                let c = report.counts;
                let timing = mic_protocol::diagnostics::TxTiming {
                    generated_packets: c.generated_packets,
                    successfully_sent_packets: c.successfully_sent_packets,
                    send_errors: c.send_errors,
                    generated_samples: c.generated_samples,
                    missed_deadlines: c.missed_deadlines,
                    sequence_gaps_generated_by_tx: c.sequence_gaps_generated_by_tx,
                    intentional_loss_drops: c.intentional_loss_drops,
                    queue_drops: c.queue_drops,
                    packets_per_second: report.actual_packets_per_second,
                    samples_per_second: report.actual_samples_per_second,
                    scheduling_drift_ms: report.scheduling_drift_ms,
                    scheduling_delay_p99_ms: report.scheduling_delay_ms.as_ref().map(|d| d.p99),
                    timing_mode: format!("{:?}", report.timing_mode),
                };
                let mut message = mic_protocol::diagnostics::Diagnostic {
                    version: 1,
                    kind: "TX_TIMING_STATS".into(),
                    device_id: device_id.clone(),
                    session_id: report.session_id.clone(),
                    sequence,
                    timing,
                    proof: String::new(),
                };
                message.sign(&state.key);
                if let Ok(bytes) = serde_json::to_vec(&message) {
                    let _ = diagnostic_socket.send_to(&bytes, peer);
                }
            }

            if let Some(file) = &mut file {
                serde_json::to_writer(&mut *file, &report)?;
                file.write_all(b"\n")?;
                file.flush()?;
            }
        }
        Ok(())
    });
    let mut clock: Option<DeadlineClock> = None;
    let mut epoch = 0;
    let mut generator = Generator::default();
    let mut counts = Counts::default();
    let mut previous_sent = 0;
    let mut previous_samples = 0;
    let mut previous_send: Option<Instant> = None;
    let mut intervals = Vec::with_capacity(256);
    let mut delays = Vec::with_capacity(256);
    let mut report_at = Instant::now();
    let mut scheduling_drift = 0.0;
    let mut queue: VecDeque<(Instant, Vec<u8>, SocketAddr)> = VecDeque::with_capacity(16);
    let mut held = None;
    let mut current_index = 0u64;
    let emit = |clock: &DeadlineClock,
                now: Instant,
                report_at: Instant,
                counts: Counts,
                previous_sent: u64,
                previous_samples: u64,
                intervals: &[f64],
                delays: &[f64],
                scheduling_drift: f64,
                epoch: u64,
                session: [u8; 16],
                final_report: bool| Report {
        timing_mode: options.mode,
        mmcss_active,
        stream_epoch: epoch,
        session_id: hex::encode(session),
        elapsed_seconds: now.duration_since(clock.start).as_secs_f64(),
        interval_seconds: now.duration_since(report_at).as_secs_f64(),
        actual_packets_per_second: (counts.successfully_sent_packets - previous_sent) as f64
            / now.duration_since(report_at).as_secs_f64().max(0.000001),
        actual_samples_per_second: (counts.generated_samples - previous_samples) as f64
            / now.duration_since(report_at).as_secs_f64().max(0.000001),
        send_interval_ms: distribution(intervals),
        scheduling_delay_ms: distribution(delays),
        scheduling_drift_ms: scheduling_drift,
        counts,
        send_interval_samples_ms: intervals.to_vec(),
        scheduling_delay_samples_ms: delays.to_vec(),
        final_report,
    };
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let state = settings.lock().unwrap().clone();
        if !state.running {
            if let Some(c) = clock.take() {
                let report = emit(
                    &c,
                    Instant::now(),
                    report_at,
                    counts,
                    previous_sent,
                    previous_samples,
                    &intervals,
                    &delays,
                    scheduling_drift,
                    epoch,
                    state.session,
                    true,
                );
                let _ = log_tx.try_send(report);
            }
            queue.clear();
            held = None;
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if clock.is_none() || epoch != state.epoch {
            epoch = state.epoch;
            clock = Some(DeadlineClock::new(Instant::now()));
            generator = Generator::default();
            counts = Counts::default();
            previous_sent = 0;
            previous_samples = 0;
            previous_send = None;
            intervals.clear();
            delays.clear();
            queue.clear();
            held = None;
            current_index = 0;
            report_at = Instant::now();
            runtime.block_on(async { ticker.reset_immediately() });
        }
        let c = clock.as_mut().unwrap();
        if options.mode == Mode::Current {
            runtime.block_on(ticker.tick());
        } else if let Some(waiter) = &waiter {
            waiter.until(c.deadline())?;
        } else {
            std::thread::sleep(c.deadline().saturating_duration_since(Instant::now()));
        }
        let now = Instant::now();
        let (slot, skipped, delay) = if options.mode == Mode::Current {
            let slot = current_index;
            current_index += 1;
            c.next_index = current_index;
            (
                slot,
                0,
                now.saturating_duration_since(c.start + Duration::from_millis(slot * 10)),
            )
        } else {
            c.due(now)
        };
        counts.missed_deadlines += u64::from(delay >= PERIOD);
        counts.sequence_gaps_generated_by_tx += skipped;
        scheduling_drift = now.duration_since(c.start).as_secs_f64() * 1000.0 - slot as f64 * 10.0;
        if delays.len() < 256 {
            delays.push(delay.as_secs_f64() * 1000.0);
        }
        // Holding this short gate through nonblocking send guarantees no packet after STOP ACK.
        let gate = settings.lock().unwrap();
        if !gate.running || gate.epoch != epoch {
            continue;
        }
        if options.mode != Mode::Current
            && previous_send.is_some_and(|p| now.duration_since(p) < Duration::from_millis(5))
        {
            counts.sequence_gaps_generated_by_tx += 1;
            continue;
        }
        if generator.sample != slot * 160 {
            generator.sample = slot * 160;
            let t = generator.sample as f64 / 16000.0;
            generator.phase = match gate.source {
                Source::Sweep => {
                    let t = t % 5.0;
                    (200.0 * t + 180.0 * t * t) % 1.0
                }
                _ => (440.0 * t) % 1.0,
            };
        }
        generator.source = gate.source.clone();
        let p = AudioPacket {
            sequence: slot as u32,
            timestamp: slot.wrapping_mul(160) as u32,
            session: gate.session,
            samples: generator.frame(gate.gain, gate.mute),
        };
        counts.generated_packets += 1;
        counts.generated_samples += 160;
        if let Some(peer) = gate.peer {
            if options.loss > 0 && (slot + 1).is_multiple_of(options.loss as u64) {
                counts.intentional_loss_drops += 1;
            } else {
                let due = now
                    + Duration::from_millis(if options.jitter_ms == 0 {
                        0
                    } else {
                        rand::random::<u64>() % (options.jitter_ms + 1)
                    });
                let bytes = p.encode(&gate.key);
                if options.reorder > 0 && (slot + 1).is_multiple_of(options.reorder as u64) {
                    held = Some((due, bytes, peer));
                } else {
                    if queue.len() < 16 {
                        queue.push_back((due, bytes, peer));
                    } else {
                        counts.queue_drops += 1;
                    }
                    if let Some(p) = held.take() {
                        if queue.len() < 16 {
                            queue.push_back(p);
                        } else {
                            counts.queue_drops += 1;
                        }
                    }
                }
            }
        }
        while queue.front().is_some_and(|p| p.0 <= Instant::now()) {
            let (_, bytes, peer) = queue.pop_front().unwrap();
            match socket.send_to(&bytes, peer) {
                Ok(n) if n == bytes.len() => {
                    let sent = Instant::now();
                    counts.successfully_sent_packets += 1;
                    if let Some(previous) = previous_send
                        && intervals.len() < 256
                    {
                        intervals.push(sent.duration_since(previous).as_secs_f64() * 1000.0);
                    }
                    previous_send = Some(sent);
                }
                _ => counts.send_errors += 1,
            }
        }
        let session = gate.session;
        drop(gate);
        if now.duration_since(report_at) >= Duration::from_secs(1) {
            let report = emit(
                c,
                now,
                report_at,
                counts,
                previous_sent,
                previous_samples,
                &intervals,
                &delays,
                scheduling_drift,
                epoch,
                session,
                false,
            );
            if log_tx.try_send(report).is_err() {
                counts.diagnostics_dropped += 1;
            }
            previous_sent = counts.successfully_sent_packets;
            previous_samples = counts.generated_samples;
            report_at = now;
            intervals.clear();
            delays.clear();
        }
    }
    if let Some(c) = clock {
        let state = settings.lock().unwrap();
        let report = emit(
            &c,
            Instant::now(),
            report_at,
            counts,
            previous_sent,
            previous_samples,
            &intervals,
            &delays,
            scheduling_drift,
            epoch,
            state.session,
            true,
        );
        let _ = log_tx.try_send(report);
    }
    drop(log_tx);
    logger
        .join()
        .map_err(|_| anyhow::anyhow!("diagnostics writer panicked"))??;
    Ok(())
}
