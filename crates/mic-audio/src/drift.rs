//! Stage A only: bounded worker-side aggregation and retention. No audio control.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
};

#[derive(Default)]
struct Stat {
    n: u64,
    sum: f64,
    min: f64,
    max: f64,
}
impl Stat {
    fn add(&mut self, v: f64) {
        if self.n == 0 {
            self.min = v;
            self.max = v;
        }
        self.n += 1;
        self.sum += v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }
    fn value(&self) -> Value {
        json!({"count":self.n,"sum":self.sum,"mean":self.sum/self.n.max(1) as f64,"min":self.min,"max":self.max})
    }
}
#[derive(Default)]
struct Bucket {
    identity: Value,
    counts: BTreeMap<String, u64>,
    stats: BTreeMap<String, Stat>,
    source_first: Option<Value>,
    source_last: Option<Value>,
    source_envelope: Option<Value>,
    envelope_residual: f64,
}
#[derive(Default)]
pub(crate) struct Summaries {
    buckets: BTreeMap<(u64, String), Bucket>,
    faults: BTreeMap<u64, BTreeSet<String>>,
    health: Option<Value>,
    discarded: u64,
    health_by_second: BTreeMap<u64, (u64, Value)>,
    closed_before: u64,
}
impl Summaries {
    pub fn add(&mut self, r: &Value) {
        let Some(t) = r["rx_monotonic_us"].as_u64() else {
            return;
        };
        let sec = t / 1_000_000;
        if sec < self.closed_before {
            self.discarded += 1;
            return;
        }
        let kind = r["record_type"].as_str().unwrap_or("event");
        if r.get("event_type").is_some() {
            self.faults
                .entry(sec)
                .or_default()
                .insert(r["event_type"].to_string());
        }
        if kind == "diagnostic_health" {
            if r["enabled"] == false
                || r["event_drops"].as_u64().unwrap_or(0) > 0
                || r["observation_drops"].as_u64().unwrap_or(0) > 0
                || !r["log_error"].is_null()
            {
                self.faults
                    .entry(sec)
                    .or_default()
                    .insert("diagnostic_coverage_incomplete".into());
            }
            if let Some(h) = &self.health
                && h["enabled_changes"] != r["enabled_changes"]
            {
                self.faults
                    .entry(sec)
                    .or_default()
                    .insert("diagnostics_toggled".into());
            }
            self.health = Some(r.clone());
            let h = self.health_by_second.entry(sec).or_insert((0, Value::Null));
            h.0 += 1;
            h.1 = r.clone();
            while self.health_by_second.len() > 8 {
                self.health_by_second.pop_first();
            }
        }
        if kind == "playback_clear"
            || kind == "stream_reset"
            || kind == "stream_start"
            || r["reason"] == "playout_anchor_changed"
        {
            self.faults.entry(sec).or_default().insert(kind.into());
        }
        if r["pending_sequence_holes"].as_u64().unwrap_or(0) > 0
            || r["duplicate"] == true
            || r["out_of_order"] == true
        {
            self.faults
                .entry(sec)
                .or_default()
                .insert("sequence_not_steady".into());
        }
        if r["output_epoch_end"].is_number() && r["output_epoch"] != r["output_epoch_end"] {
            self.faults
                .entry(sec)
                .or_default()
                .insert("callback_crossed_output_epoch".into());
        }
        if r["inactive_frames"].as_u64().unwrap_or(0) > 0
            || r["prefill_frames"].as_u64().unwrap_or(0) > 0
            || r["missing_input_samples"].as_u64().unwrap_or(0) > 0
        {
            self.faults
                .entry(sec)
                .or_default()
                .insert("callback_not_steady".into());
        }
        if r["dropped_samples"].as_u64().unwrap_or(0) > 0
            || r["discarded_samples"].as_u64().unwrap_or(0) > 0
        {
            self.faults
                .entry(sec)
                .or_default()
                .insert("sample_discard".into());
        }
        let identity = json!({"clock_id":r["clock_id"],"session_id":r["session_id"],"stream_id":r["stream_id"],"playout_epoch":r["playout_epoch"],"output_epoch":r["output_epoch"]});
        let key = (sec, identity.to_string());
        let b = self.buckets.entry(key).or_default();
        b.identity = identity;
        *b.counts.entry(kind.into()).or_default() += 1;
        if kind == "udp_receive" && r["duplicate"] == false {
            *b.counts
                .entry("unique_authenticated_packets".into())
                .or_default() += 1;
        }
        if kind == "jitter_dequeue" && r["operational"] == true {
            *b.counts.entry("logical_playout_slots".into()).or_default() += 1;
        }
        for field in [
            "occupancy",
            "contiguous_depth",
            "ring_before",
            "ring_after",
            "requested_frames",
            "consumed_input_samples",
            "supplied_frames",
            "inserted_frames",
            "accepted_samples",
            "dropped_samples",
            "discarded_samples",
            "overdue_us",
            "output_rate",
        ] {
            if let Some(v) = r[field].as_f64() {
                b.stats.entry(format!("{kind}.{field}")).or_default().add(v);
            }
        }
        if kind == "udp_receive"
            && r["duplicate"] == false
            && r["out_of_order"] == false
            && r["source_sample_index"].is_number()
        {
            let point = json!({"rx_monotonic_us":t,"source_sample_index":r["source_sample_index"],"packet_sequence":r["packet_sequence"]});
            if b.source_first
                .as_ref()
                .is_none_or(|p| p["rx_monotonic_us"].as_u64().unwrap() > t)
            {
                b.source_first = Some(point.clone());
            }
            let first = b.source_first.as_ref().unwrap();
            let delta = (r["source_sample_index"].as_u64().unwrap() as u32)
                .wrapping_sub(first["source_sample_index"].as_u64().unwrap() as u32);
            let residual = t as f64
                - first["rx_monotonic_us"].as_u64().unwrap() as f64
                - delta as f64 * 1_000_000.0 / 16000.0;
            if b.source_envelope.is_none() || residual < b.envelope_residual {
                b.source_envelope = Some(point.clone());
                b.envelope_residual = residual;
            }
            if b.source_last
                .as_ref()
                .is_none_or(|p| p["rx_monotonic_us"].as_u64().unwrap() < t)
            {
                b.source_last = Some(point);
            }
        }
        while self.buckets.len() > 64 {
            self.buckets.pop_first();
            self.discarded += 1;
        }
        while self.faults.len() > 8 {
            self.faults.pop_first();
        }
    }
    pub fn finish_before(&mut self, second: u64) -> Vec<Value> {
        let keys: Vec<_> = self
            .buckets
            .keys()
            .filter(|(s, _)| *s < second)
            .cloned()
            .collect();
        let mut rows = Vec::new();
        for key in keys {
            let b = self.buckets.remove(&key).unwrap();
            let mut row = b.identity;
            row["record_type"] = "drift_summary".into();
            row["window_start_us"] = (key.0 * 1_000_000).into();
            row["window_end_us"] = ((key.0 + 1) * 1_000_000).into();
            row["counts"] = json!(b.counts);
            row["statistics"] = json!(
                b.stats
                    .iter()
                    .map(|(k, s)| (k, s.value()))
                    .collect::<BTreeMap<_, _>>()
            );
            row["source_first"] = json!(b.source_first);
            row["source_last"] = json!(b.source_last);
            row["source_envelope"] = json!(b.source_envelope);
            row["invalid_reasons"] = json!(self.faults.get(&key.0).cloned().unwrap_or_default());
            row["discarded_summary_buckets_total"] = self.discarded.into();
            let health = self.health_by_second.get(&key.0);
            row["diagnostic_health"] = json!(health.map(|h| &h.1));
            row["diagnostic_health_count"] = json!(health.map(|h| h.0).unwrap_or(0));
            if health.is_none_or(|h| h.0 < 5) {
                row["invalid_reasons"]
                    .as_array_mut()
                    .unwrap()
                    .push("health_coverage_insufficient".into());
            }
            row["window_semantics"]="1-second half-open RX buckets; admitted observations; means are observation-weighted, not time-weighted".into();
            rows.push(row);
        }
        self.faults.retain(|s, _| *s >= second);
        self.health_by_second.retain(|s, _| *s >= second);
        self.closed_before = second;
        rows
    }
}

pub(crate) struct Sink {
    dir: PathBuf,
    stem: String,
    detail: Option<File>,
    parts: VecDeque<Value>,
    index: u64,
    bytes: usize,
    part_limit: usize,
    retain: usize,
    summary: Option<File>,
    summary_bytes: usize,
    events: Option<File>,
    event_bytes: usize,
    reserve_limit: usize,
    evicted_through: Option<u64>,
    dirty: bool,
}
impl Sink {
    pub fn new(dir: PathBuf, clock: String) -> Self {
        Self {
            dir,
            stem: format!("events-{clock}"),
            detail: None,
            parts: VecDeque::new(),
            index: 0,
            bytes: 0,
            part_limit: 8 * 1024 * 1024,
            retain: 8,
            summary: None,
            summary_bytes: 0,
            events: None,
            event_bytes: 0,
            reserve_limit: 32 * 1024 * 1024,
            evicted_through: None,
            dirty: false,
        }
    }
    fn path(&self, suffix: &str) -> PathBuf {
        self.dir.join(format!("{}-{suffix}", self.stem))
    }
    fn open(&self, path: &PathBuf) -> std::io::Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        OpenOptions::new().create_new(true).write(true).open(path)
    }
    pub fn detail(&mut self, r: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(r)?;
        line.push(b'\n');
        if r.get("event_type").is_some() {
            if self.event_bytes + line.len() > self.reserve_limit {
                return Err(std::io::Error::other(
                    "fault archive cap reached; event counts continue in summaries",
                ));
            }
            if self.events.is_none() {
                self.events = Some(self.open(&self.path("faults.jsonl"))?);
            }
            self.events.as_mut().unwrap().write_all(&line)?;
            self.event_bytes += line.len();
        }
        if self.detail.is_none() || self.bytes + line.len() > self.part_limit {
            self.detail.take();
            self.index += 1;
            self.bytes = 0;
            let path = self.path(&format!("{:06}.jsonl", self.index));
            self.detail = Some(self.open(&path)?);
            self.parts.push_back(
                json!({"path":path,"first_us":r["rx_monotonic_us"],"last_us":r["rx_monotonic_us"]}),
            );
            while self.parts.len() > self.retain {
                // Only files created by this Sink, inside its immutable directory.
                let old = self.parts.front().unwrap();
                let old_path = PathBuf::from(old["path"].as_str().unwrap());
                if old_path.parent() != Some(self.dir.as_path()) {
                    return Err(std::io::Error::other("invalid retention path"));
                }
                std::fs::remove_file(&old_path)?;
                let old = self.parts.pop_front().unwrap();
                self.evicted_through = old["last_us"].as_u64();
            }
            self.dirty = true;
        }
        self.detail.as_mut().unwrap().write_all(&line)?;
        self.bytes += line.len();
        let part = self.parts.back_mut().unwrap();
        if let Some(t) = r["rx_monotonic_us"].as_u64() {
            part["first_us"] = part["first_us"].as_u64().unwrap_or(t).min(t).into();
            part["last_us"] = part["last_us"].as_u64().unwrap_or(t).max(t).into();
        }
        Ok(())
    }
    pub fn summary(&mut self, r: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(r)?;
        line.push(b'\n');
        if self.summary_bytes + line.len() > self.reserve_limit {
            return Err(std::io::Error::other(
                "drift summary cap reached; later windows unavailable",
            ));
        }
        if self.summary.is_none() {
            self.summary = Some(self.open(&self.path("summary.jsonl"))?);
            self.dirty = true;
        }
        self.summary.as_mut().unwrap().write_all(&line)?;
        self.summary_bytes += line.len();
        Ok(())
    }
    pub fn checkpoint(&mut self) {
        self.dirty = true;
    }
    pub fn manifest(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let value = json!({"record_type":"drift_manifest","detail_parts":self.parts,"detail_evicted_through_us":self.evicted_through,"summary_path":self.path("summary.jsonl"),"fault_archive_path":self.path("faults.jsonl"),"detail_retention_bytes":self.part_limit*self.retain,"summary_limit_bytes":self.reserve_limit,"fault_limit_bytes":self.reserve_limit,"tail_may_be_unflushed":true});
        let temp = self.path("manifest.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(&value)?)?;
        std::fs::rename(temp, self.path("manifest.json"))?;
        self.dirty = false;
        Ok(())
    }
    pub fn current_path(&self) -> Option<String> {
        self.parts
            .back()
            .and_then(|p| p["path"].as_str())
            .map(str::to_owned)
    }
    pub fn manifest_path(&self) -> Option<String> {
        self.parts
            .front()
            .map(|_| self.path("manifest.json").display().to_string())
    }
    pub fn summary_path(&self) -> Option<String> {
        self.summary
            .as_ref()
            .map(|_| self.path("summary.jsonl").display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_keep_population_and_are_bounded() {
        let mut s = Summaries::default();
        for t in 0..1800 {
            s.add(&json!({"record_type":"jitter_dequeue","rx_monotonic_us":t*1_000_000,"occupancy":3,"stream_id":1}));
            if t > 2 {
                let rows = s.finish_before(t - 2);
                assert!(rows.len() <= 1);
            }
        }
        assert!(s.buckets.len() <= 4);
        let rows = s.finish_before(2000);
        assert_eq!(
            rows[0]["statistics"]["jitter_dequeue.occupancy"]["mean"],
            3.0
        );
        assert!(rows[0]["source_last"].is_null());
    }
    #[test]
    fn rotation_reserves_summary_and_faults_and_reports_eviction() {
        let dir = std::env::temp_dir().join(format!("mic-drift-{}", crate::diagnostics::wall_ms()));
        let mut sink = Sink::new(dir.clone(), "test".into());
        sink.part_limit = 100;
        sink.retain = 2;
        for t in 0..20 {
            sink.detail(&json!({"event_type":"BUFFER_UNDERRUN","rx_monotonic_us":t}))
                .unwrap();
        }
        sink.summary(&json!({"record_type":"drift_summary"}))
            .unwrap();
        sink.manifest().unwrap();
        assert_eq!(sink.parts.len(), 2);
        assert!(sink.evicted_through.is_some());
        let events = std::fs::read_to_string(sink.path("faults.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 20);
        sink.reserve_limit = 1;
        assert!(sink.summary(&json!({"large":true})).is_err());
        drop(sink);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disk_failure_and_late_records_cannot_be_silent_success() {
        let path =
            std::env::temp_dir().join(format!("mic-drift-file-{}", crate::diagnostics::wall_ms()));
        std::fs::write(&path, b"not a directory").unwrap();
        let mut sink = Sink::new(path.clone(), "test".into());
        assert!(
            sink.detail(&json!({"rx_monotonic_us":1,"record_type":"test"}))
                .is_err()
        );
        std::fs::remove_file(path).unwrap();
        let mut summaries = Summaries::default();
        summaries.finish_before(10);
        summaries.add(&json!({"rx_monotonic_us":1,"record_type":"udp_receive"}));
        assert_eq!(summaries.discarded, 1);
    }
}
