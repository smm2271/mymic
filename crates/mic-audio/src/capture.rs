//! Optional pre-resampler PCM capture. Producers never perform disk I/O.
use crate::diagnostics::{Diagnostics, diagnostics_dir, wall_ms};
use crossbeam_queue::ArrayQueue;
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
const MAX_SAMPLES: u32 = 16000 * 30;
#[derive(Clone, Default, Debug, Serialize)]
pub struct Status {
    pub active: bool,
    pub path: Option<String>,
    pub metadata_path: Option<String>,
    pub started_monotonic_us: Option<u64>,
    pub ended_monotonic_us: Option<u64>,
    pub started_unix_ms: Option<u128>,
    pub ended_unix_ms: Option<u128>,
    pub samples: u32,
    pub dropped_frames: u64,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub capture_point: &'static str,
    pub stream_id: u64,
    pub clock_id: String,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub first_frame_monotonic_us: Option<u64>,
    pub last_frame_monotonic_us: Option<u64>,
    pub event_drop_count_start: u64,
    pub event_drop_count_end: u64,
    pub observation_drop_count_start: u64,
    pub observation_drop_count_end: u64,
    pub event_log_path: Option<String>,
    pub event_log_error: Option<String>,
}
#[derive(Clone, Copy)]
struct Frame {
    generation: u64,
    timestamp_us: u64,
    stream: u64,
    samples: [i16; 160],
}
enum Command {
    Start(mpsc::Sender<Result<Status, String>>),
    Stop(mpsc::Sender<Result<Status, String>>),
}
pub struct Capture {
    queue: ArrayQueue<Frame>,
    active: AtomicU64,
    drops: AtomicU64,
    state: Mutex<Status>,
    commands: mpsc::SyncSender<Command>,
    diagnostics: Arc<Diagnostics>,
    directory: PathBuf,
}
#[derive(Serialize)]
struct Position {
    sample_offset: u32,
    rx_monotonic_us: u64,
}
#[derive(Serialize)]
struct Metadata<'a> {
    #[serde(flatten)]
    status: &'a Status,
    frames: &'a [Position],
}
struct Recording {
    positions: Vec<Position>,
    writer: Wav,
    deadline: Instant,
    generation: u64,
    drop_start: u64,
    metadata: PathBuf,
}
impl Capture {
    pub fn new(diagnostics: Arc<Diagnostics>) -> Arc<Self> {
        Self::spawn(diagnostics, diagnostics_dir())
    }
    fn spawn(diagnostics: Arc<Diagnostics>, directory: PathBuf) -> Arc<Self> {
        let (capture, rx) = Self::bounded(diagnostics, 128, directory);
        let weak = Arc::downgrade(&capture);
        std::thread::spawn(move || {
            let mut recording: Option<Recording> = None;
            let mut generation = 0u64;
            while let Some(c) = weak.upgrade() {
                while let Ok(command) = rx.try_recv() {
                    match command {
                        Command::Start(reply) => {
                            let result = if recording.is_some() {
                                Err("capture already active".into())
                            } else {
                                generation += 1;
                                c.begin(generation).map(|r| {
                                    recording = Some(r);
                                    c.status()
                                })
                            };
                            let _ = reply.send(result);
                        }
                        Command::Stop(reply) => {
                            c.active.store(0, Ordering::Release);
                            c.consume(&mut recording);
                            let reason = c.finish_reason(&recording).unwrap_or("manual_stop");
                            c.finish(&mut recording, reason);
                            let _ = reply.send(Ok(c.status()));
                        }
                    }
                }
                c.consume(&mut recording);
                if let Some(reason) = c.finish_reason(&recording) {
                    c.finish(&mut recording, reason);
                }
                drop(c);
                std::thread::sleep(Duration::from_millis(5));
            }
            // Best effort header repair if service ownership ends unexpectedly.
            if let Some(mut r) = recording {
                let _ = r.writer.finish();
            }
        });
        capture
    }
    fn finish_reason(&self, recording: &Option<Recording>) -> Option<&'static str> {
        let r = recording.as_ref()?;
        let status = self.status();
        if status.error.is_some() {
            Some("disk_error")
        } else if self.drops.load(Ordering::Relaxed) != r.drop_start {
            Some("queue_overflow_incomplete")
        } else if status.samples >= MAX_SAMPLES || Instant::now() >= r.deadline {
            Some("timeout_30s")
        } else if self.diagnostics.stream.load(Ordering::Acquire) != status.stream_id {
            Some("stream_changed")
        } else {
            None
        }
    }
    fn bounded(
        diagnostics: Arc<Diagnostics>,
        capacity: usize,
        directory: PathBuf,
    ) -> (Arc<Self>, mpsc::Receiver<Command>) {
        let (commands, rx) = mpsc::sync_channel(4);
        let capture = Arc::new(Self {
            queue: ArrayQueue::new(capacity),
            active: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            state: Mutex::new(Status {
                capture_point: "post_declick_16khz_mono_before_playback_ring_and_device_resampler",
                ..Status::default()
            }),
            commands,
            diagnostics,
            directory,
        });
        (capture, rx)
    }
    pub fn status(&self) -> Status {
        self.state.lock().unwrap().clone()
    }
    pub fn start(&self) -> Result<Status, String> {
        self.command(true)
    }
    pub fn stop(&self) -> Result<Status, String> {
        self.command(false)
    }
    fn command(&self, start: bool) -> Result<Status, String> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .try_send(if start {
                Command::Start(tx)
            } else {
                Command::Stop(tx)
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?
    }
    pub fn feed(&self, samples: [i16; 160]) {
        let generation = self.active.load(Ordering::Acquire);
        if generation == 0 {
            return;
        }
        let frame = Frame {
            generation,
            timestamp_us: self.diagnostics.now_us(),
            stream: self.diagnostics.stream.load(Ordering::Acquire),
            samples,
        };
        if self.queue.push(frame).is_err() {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn begin(&self, generation: u64) -> Result<Recording, String> {
        while self.queue.pop().is_some() {}
        let result = (|| -> std::io::Result<Recording> {
            let dir = &self.directory;
            std::fs::create_dir_all(dir)?;
            let path = dir.join(format!(
                "capture-{}-{}-{generation}.wav",
                wall_ms(),
                std::process::id()
            ));
            let metadata = path.with_extension("json");
            let writer = Wav::new(&path)?;
            let d = self.diagnostics.snapshot();
            *self.state.lock().unwrap() = Status {
                active: true,
                path: Some(path.display().to_string()),
                metadata_path: Some(metadata.display().to_string()),
                started_monotonic_us: Some(self.diagnostics.now_us()),
                started_unix_ms: Some(wall_ms()),
                stream_id: self.diagnostics.stream.load(Ordering::Acquire),
                clock_id: d.clock_id,
                device_id: d.current_device_id,
                session_id: d.current_session_id,
                event_drop_count_start: d.drop_count,
                observation_drop_count_start: d.observation_drop_count,
                event_log_path: d.event_log_path,
                event_log_error: d.event_log_error,
                capture_point: "post_declick_16khz_mono_before_playback_ring_and_device_resampler",
                ..Status::default()
            };
            Ok(Recording {
                positions: Vec::with_capacity(3000),
                writer,
                metadata,
                deadline: Instant::now() + Duration::from_secs(30),
                generation,
                drop_start: self.drops.load(Ordering::Relaxed),
            })
        })();
        match result {
            Ok(r) => {
                self.active.store(generation, Ordering::Release);
                Ok(r)
            }
            Err(e) => {
                self.state.lock().unwrap().error = Some(e.to_string());
                Err(e.to_string())
            }
        }
    }
    fn consume(&self, recording: &mut Option<Recording>) {
        for _ in 0..self.queue.capacity() {
            let Some(frame) = self.queue.pop() else {
                break;
            };
            let Some(r) = recording.as_mut() else {
                continue;
            };
            if frame.generation != r.generation {
                continue;
            }
            let state = self.state.lock().unwrap();
            if frame.stream != state.stream_id
                || state.samples >= MAX_SAMPLES
                || state.error.is_some()
            {
                continue;
            }
            if frame.timestamp_us > state.started_monotonic_us.unwrap() + 30_000_000 {
                continue;
            }
            drop(state);
            let result = r.writer.append(&frame.samples);
            let mut state = self.state.lock().unwrap();
            match result {
                Ok(()) => {
                    r.positions.push(Position {
                        sample_offset: state.samples,
                        rx_monotonic_us: frame.timestamp_us,
                    });
                    state.samples += 160;
                    state
                        .first_frame_monotonic_us
                        .get_or_insert(frame.timestamp_us);
                    state.last_frame_monotonic_us = Some(frame.timestamp_us);
                }
                Err(e) => {
                    state.error = Some(e.to_string());
                }
            }
        }
    }
    fn finish(&self, recording: &mut Option<Recording>, reason: &str) {
        let Some(mut r) = recording.take() else {
            return;
        };
        self.active.store(0, Ordering::Release);
        let finish_result = r.writer.finish();
        let mut state = self.status();
        if let Err(e) = finish_result {
            state.error = Some(e.to_string());
        }
        state.active = false;
        state.ended_monotonic_us = Some(self.diagnostics.now_us());
        state.ended_unix_ms = Some(wall_ms());
        state.dropped_frames = self.drops.load(Ordering::Relaxed) - r.drop_start;
        state.reason = Some(reason.into());
        let d = self.diagnostics.snapshot();
        state.event_drop_count_end = d.drop_count;
        state.observation_drop_count_end = d.observation_drop_count;
        state.event_log_path = d.event_log_path;
        state.event_log_error = d.event_log_error;
        let metadata = state.clone();
        let result = (|| -> std::io::Result<()> {
            let bytes = serde_json::to_vec_pretty(&Metadata {
                status: &metadata,
                frames: &r.positions,
            })?;
            let mut f = File::create(&r.metadata)?;
            f.write_all(&bytes)
        })();
        if let Err(e) = result {
            state.error = Some(format!("metadata: {e}"));
        }
        *self.state.lock().unwrap() = state;
    }
}
struct Wav {
    file: File,
    samples: u32,
}
impl Wav {
    fn new(path: &std::path::Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&header(0))?;
        Ok(Self { file, samples: 0 })
    }
    fn append(&mut self, samples: &[i16; 160]) -> std::io::Result<()> {
        let mut bytes = [0; 320];
        for (i, sample) in samples.iter().enumerate() {
            bytes[2 * i..2 * i + 2].copy_from_slice(&sample.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        self.samples += 160;
        Ok(())
    }
    fn finish(&mut self) -> std::io::Result<()> {
        self.file.set_len(44 + self.samples as u64 * 2)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.samples))?;
        self.file.flush()
    }
}
fn header(samples: u32) -> [u8; 44] {
    let mut b = [0; 44];
    b[0..4].copy_from_slice(b"RIFF");
    b[4..8].copy_from_slice(&(36 + samples * 2).to_le_bytes());
    b[8..16].copy_from_slice(b"WAVEfmt ");
    b[16..20].copy_from_slice(&16u32.to_le_bytes());
    b[20..22].copy_from_slice(&1u16.to_le_bytes());
    b[22..24].copy_from_slice(&1u16.to_le_bytes());
    b[24..28].copy_from_slice(&16000u32.to_le_bytes());
    b[28..32].copy_from_slice(&32000u32.to_le_bytes());
    b[32..34].copy_from_slice(&2u16.to_le_bytes());
    b[34..36].copy_from_slice(&16u16.to_le_bytes());
    b[36..40].copy_from_slice(b"data");
    b[40..44].copy_from_slice(&(samples * 2).to_le_bytes());
    b
}
#[cfg(test)]
mod tests {
    use super::*;
    fn directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mic-capture-{name}-{}-{}",
            std::process::id(),
            wall_ms()
        ))
    }
    #[test]
    fn commands_start_stop_restart_and_disabled_feed() {
        let dir = directory("commands");
        let d = Diagnostics::bounded(16);
        d.set_context("test-tx", "session");
        let c = Capture::spawn(d, dir.clone());
        c.feed([99; 160]);
        assert!(c.queue.is_empty());
        assert!(!c.status().active);
        let start = c.start().unwrap();
        assert!(start.active);
        assert!(c.start().is_err());
        c.feed([123; 160]);
        c.feed([-123; 160]);
        let end = c.stop().unwrap();
        assert_eq!(end.samples, 320);
        assert_eq!(end.reason.as_deref(), Some("manual_stop"));
        let bytes = std::fs::read(end.path.unwrap()).unwrap();
        assert_eq!(bytes.len(), 684);
        assert_eq!(&bytes[44..46], &123i16.to_le_bytes());
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(end.metadata_path.unwrap()).unwrap()).unwrap();
        assert_eq!(metadata["frames"].as_array().unwrap().len(), 2);
        assert_eq!(metadata["frames"][1]["sample_offset"], 160);
        assert_eq!(metadata["device_id"], "test-tx");
        c.start().unwrap();
        let second = c.stop().unwrap();
        assert_eq!(second.samples, 0);
        c.feed([1; 160]);
        assert!(c.queue.is_empty());
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn overflow_timeout_sample_cap_stream_change_and_disk_error() {
        let dir = directory("boundaries");
        let d = Diagnostics::bounded(16);
        let (c, _rx) = Capture::bounded(d.clone(), 1, dir.clone());
        let mut r = Some(c.begin(1).unwrap());
        c.feed([1; 160]);
        c.feed([2; 160]);
        assert_eq!(c.finish_reason(&r), Some("queue_overflow_incomplete"));
        c.consume(&mut r);
        c.finish(&mut r, "queue_overflow_incomplete");
        assert_eq!(c.status().dropped_frames, 1);
        assert_eq!(c.status().samples, 160);
        r = Some(c.begin(2).unwrap());
        r.as_mut().unwrap().deadline = Instant::now();
        assert_eq!(c.finish_reason(&r), Some("timeout_30s"));
        c.finish(&mut r, "timeout_30s");
        r = Some(c.begin(3).unwrap());
        for _ in 0..3001 {
            c.feed([3; 160]);
            c.consume(&mut r);
        }
        assert_eq!(c.status().samples, MAX_SAMPLES);
        assert_eq!(c.finish_reason(&r), Some("timeout_30s"));
        c.finish(&mut r, "timeout_30s");
        assert_eq!(
            std::fs::metadata(c.status().path.unwrap()).unwrap().len(),
            960044
        );
        r = Some(c.begin(4).unwrap());
        d.set_context("other", "new");
        c.feed([9; 160]);
        c.consume(&mut r);
        assert_eq!(c.status().samples, 0);
        assert_eq!(c.finish_reason(&r), Some("stream_changed"));
        c.finish(&mut r, "stream_changed");
        // A read-only file handle reliably injects a write error without requiring a full disk.
        r = Some(c.begin(5).unwrap());
        r.as_mut().unwrap().writer.file = File::open(c.status().path.as_ref().unwrap()).unwrap();
        c.feed([4; 160]);
        c.consume(&mut r);
        assert_eq!(c.finish_reason(&r), Some("disk_error"));
        c.finish(&mut r, "disk_error");
        assert!(c.status().error.is_some());
        let blocked = dir.join("not-a-directory");
        std::fs::write(&blocked, b"x").unwrap();
        let (bad, _rx) = Capture::bounded(d, 1, blocked);
        assert!(bad.begin(1).is_err());
        assert!(!bad.status().active);
        drop(r);
        drop(c);
        drop(bad);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn wav_header_and_length() {
        let path =
            std::env::temp_dir().join(format!("mic-wav-{}-{}.wav", std::process::id(), wall_ms()));
        let mut wav = Wav::new(&path).unwrap();
        for _ in 0..100 {
            wav.append(&[-123; 160]).unwrap();
        }
        wav.finish().unwrap();
        drop(wav);
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len(), 32044);
        assert_eq!(&b[..44], &header(16000));
        assert_eq!(&b[44..46], &(-123i16).to_le_bytes());
        std::fs::remove_file(path).unwrap();
    }
}
