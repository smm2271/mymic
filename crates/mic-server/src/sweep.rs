use crate::sweep_model::*;
use mic_network::{Action, Core};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
#[derive(Clone)]
pub struct Sweep {
    pub data: Arc<Mutex<Value>>,
    pub gate: Arc<Mutex<()>>,
    root: Arc<PathBuf>,
}
pub fn utc() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
impl Sweep {
    pub fn new() -> Result<Self, String> {
        let root = PathBuf::from(std::env::var("MIC_LAB_DIR").unwrap_or("data/audio-lab".into()));
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let path = root.join("suite.json");
        let mut data = if path.exists() {
            serde_json::from_slice::<Value>(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            json!({"schema_version":2,"status":"IDLE","runs":[]})
        };
        if !["IDLE", "COMPLETED", "FAILED", "CANCELLED"]
            .contains(&data["status"].as_str().unwrap_or(""))
        {
            data["status"] = "FAILED".into();
            data["error"] = "RX restarted; incomplete run interrupted".into();
            if let Some(mut r) = data.get("current_run").filter(|r| r.is_object()).cloned() {
                r["validity"] = "INTERRUPTED".into();
                r["invalid_reasons"] = json!([{"code":"RX_RESTART"}]);
                data["runs"].as_array_mut().unwrap().push(r);
            }
            data["current_run"] = Value::Null;
            data["comparison_summary"] = comparison(data["runs"].as_array().unwrap());
        }
        let bytes = serde_json::to_vec_pretty(&data).unwrap();
        std::fs::write(root.join("recovery.tmp"), &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(root.join("recovery.tmp"), root.join("suite.json"))
            .map_err(|e| e.to_string())?;
        if let Some(id) = data["suite_id"].as_str() {
            if id.starts_with("suite-") && id[6..].chars().all(|c| c.is_ascii_digit()) {
                std::fs::write(root.join("recovery.tmp"), &bytes).map_err(|e| e.to_string())?;
                std::fs::rename(root.join("recovery.tmp"), root.join(format!("{id}.json")))
                    .map_err(|e| e.to_string())?;
            }
        }
        let s = Self {
            data: Arc::new(Mutex::new(data)),
            gate: Arc::new(Mutex::new(())),
            root: Arc::new(root),
        };
        Ok(s)
    }
    pub fn history(&self) -> Result<Vec<String>, String> {
        let mut ids = vec![];
        for e in std::fs::read_dir(&*self.root).map_err(|e| e.to_string())? {
            let name = e
                .map_err(|e| e.to_string())?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.starts_with("suite-") && name.ends_with(".json") {
                ids.push(name.trim_end_matches(".json").into());
            }
        }
        ids.sort();
        ids.reverse();
        Ok(ids)
    }
    pub fn read_archive(&self, id: &str) -> Result<Value, String> {
        if !id.starts_with("suite-") || !id[6..].chars().all(|c| c.is_ascii_digit()) {
            return Err("invalid suite id".into());
        }
        serde_json::from_slice(
            &std::fs::read(self.root.join(format!("{id}.json"))).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub async fn persist(&self) -> Result<(), String> {
        let d = self.data.lock().await;
        let temp = self.root.join("suite.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(&*d).unwrap())
            .map_err(|e| e.to_string())?;
        std::fs::rename(temp, self.root.join("suite.json")).map_err(|e| e.to_string())?;
        if let Some(id) = d["suite_id"].as_str() {
            if id.starts_with("suite-") && id[6..].chars().all(|c| c.is_ascii_digit()) {
                std::fs::write(
                    self.root.join(format!("{id}.tmp")),
                    serde_json::to_vec_pretty(&*d).unwrap(),
                )
                .map_err(|e| e.to_string())?;
                std::fs::rename(
                    self.root.join(format!("{id}.tmp")),
                    self.root.join(format!("{id}.json")),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub async fn state(&self, status: &str) {
        self.data.lock().await["status"] = status.into();
    }
    pub async fn active(&self) -> bool {
        !matches!(
            self.data.lock().await["status"].as_str(),
            Some("IDLE" | "COMPLETED" | "FAILED" | "CANCELLED")
        )
    }
    pub async fn cancelled(&self) -> bool {
        self.data.lock().await["cancel_requested"] == true
    }
    pub async fn save_plan(&self, p: &Plan) -> Result<(), String> {
        std::fs::write(
            self.root.join("plan.json"),
            serde_json::to_vec_pretty(p).unwrap(),
        )
        .map_err(|e| e.to_string())
    }
    pub fn load_plan(&self) -> Result<Value, String> {
        serde_json::from_slice(
            &std::fs::read(self.root.join("plan.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub async fn start(&self, core: Core, mut p: Plan) -> Result<(), String> {
        let _g = self.gate.lock().await;
        if self.active().await {
            return Err("suite already active".into());
        }
        p.validate()?;
        let s = core.snapshot.read().await;
        let dev = s
            .devices
            .iter()
            .find(|d| d.id == p.device_id)
            .ok_or("device not found")?;
        if !dev
            .capabilities
            .iter()
            .any(|c| c == mic_protocol::lab::CAPABILITY)
        {
            return Err("device does not support Audio Lab".into());
        }
        drop(s);
        self.save_plan(&p).await?;
        *self.data.lock().await = json!({"schema_version":2,"suite_id":format!("suite-{}",utc()),"plan_id":p.plan_id,"plan":p,"status":"PREPARING","runs":[],"current_run":null,"cancel_requested":false,"continue_requested":false,"total_runs":p.execution_order.len()});
        if let Err(e) = self.persist().await {
            self.state("FAILED").await;
            return Err(e);
        }
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.execute(core, p).await {
                this.data.lock().await["error"] = e.into();
                this.state("FAILED").await;
            }
            if let Err(e) = this.persist().await {
                this.data.lock().await["error"] = e.into();
                this.state("FAILED").await;
            }
        });
        Ok(())
    }
    async fn observation(core: &Core) -> Value {
        let s = core.snapshot.read().await;
        let mut v = serde_json::to_value(&*s).unwrap();
        v["diagnostics"] = json!(core.playback.diagnostics.snapshot());
        v
    }
    async fn execute(&self, core: Core, p: Plan) -> Result<(), String> {
        for (index, (id, rep)) in p.execution_order.iter().enumerate() {
            if self.cancelled().await {
                break;
            }
            let c = p
                .selected_combinations
                .iter()
                .find(|c| &c.combination_id == id)
                .unwrap();
            let before = Self::observation(&core).await;
            self.state("APPLYING_CONFIG").await;
            let mut r = json!({"run_id":format!("run-{}-{index}",utc()),"plan_id":p.plan_id,"combination_id":id,"repetition_index":rep,"device_id":p.device_id,"requested_config":c.config,"rx_version":env!("MIC_BUILD_COMMIT"),"tx_version":before["devices"].as_array().and_then(|ds|ds.iter().find(|d|d["id"]==p.device_id)).map(|d|d["capabilities"].clone()),"validity":"INVALID","invalid_reasons":[],"started_at":utc()});
            self.data.lock().await["current_run"] = r.clone();
            self.persist().await?;
            let apply = core
                .command(p.device_id.clone(), Action::Transport(c.config))
                .await;
            let mut reason = apply
                .err()
                .map(|e| json!({"code":"CONFIG_ACK_FAILED","detail":e}));
            self.state("VERIFYING_CONFIG").await;
            let mut obs = Self::observation(&core).await;
            for _ in 0..30 {
                if obs["lab"][&p.device_id]["applied"] == json!(c.config)
                    && obs["active_device"] == p.device_id
                    && obs["diagnostic_stream_id"] != before["diagnostic_stream_id"]
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                obs = Self::observation(&core).await;
            }
            if reason.is_none()
                && (obs["lab"][&p.device_id]["applied"] != json!(c.config)
                    || obs["active_device"] != p.device_id
                    || obs["diagnostic_stream_id"] == before["diagnostic_stream_id"])
            {
                reason = Some(json!({"code":"EFFECTIVE_CONFIG_OR_NEW_STREAM_UNVERIFIED"}));
            }
            r["effective_config"] = obs["lab"][&p.device_id]["applied"].clone();
            r["session_id"] = obs["diagnostics"]["current_session_id"].clone();
            r["stream_id"] = obs["diagnostic_stream_id"].clone();
            r["clock_id"] = obs["diagnostics"]["clock_id"].clone();
            r["artifacts"] = json!({"event_log":obs["diagnostics"]["event_log_path"],"manifest":obs["diagnostics"]["drift_manifest_path"]});
            if reason.is_none() {
                self.state("WARMING_UP").await;
                if let Err(e) = self
                    .window(&core, &p, &c.config, &obs, p.global_settings.warmup_seconds)
                    .await
                {
                    reason = Some(e);
                }
            }
            if reason.is_none() {
                let start = Self::observation(&core).await;
                let a = counters(&start, &start["diagnostics"]);
                r["started_at"] = utc().into();
                r["start_counters"] = json!(a);
                r["start_monotonic_us"] = start["counter_monotonic_us"].clone();
                self.state("MEASURING").await;
                self.data.lock().await["current_run"] = r.clone();
                self.persist().await?;
                let duration = c
                    .duration_seconds
                    .unwrap_or(p.global_settings.duration_seconds);
                let result = self.window(&core, &p, &c.config, &start, duration).await;
                let end = Self::observation(&core).await;
                let b = counters(&end, &end["diagnostics"]);
                r["end_counters"] = json!(b);
                r["end_monotonic_us"] = end["counter_monotonic_us"].clone();
                let t = (end["counter_monotonic_us"]
                    .as_u64()
                    .unwrap()
                    .saturating_sub(start["counter_monotonic_us"].as_u64().unwrap()))
                    as f64
                    / 1e6;
                r["duration_seconds"] = json!(t);
                reason = result.err();
                if reason.is_none() {
                    match delta(&a, &b) {
                        Ok(d) => {
                            r["counter_delta"] = json!(d);
                            r["derived_metrics"] = derived(&d, t);
                            r["validity"] = "VALID".into();
                        }
                        Err(e) => reason = Some(json!({"code":"COUNTER_RESET","detail":e})),
                    }
                }
            }
            self.state("FINALIZING").await;
            if let Some(e) = reason {
                r["invalid_reasons"] = json!([e]);
            }
            if self.cancelled().await {
                r["validity"] = "INTERRUPTED".into();
                r["invalid_reasons"] = json!([{"code":"USER_CANCELLED"}]);
            }
            r["ended_at"] = utc().into();
            {
                let mut d = self.data.lock().await;
                d["runs"].as_array_mut().unwrap().push(r);
                d["current_run"] = Value::Null;
                d["comparison_summary"] = comparison(d["runs"].as_array().unwrap());
                let rs = d["runs"].as_array().unwrap();
                let good = rs.iter().filter(|r| r["validity"] == "VALID").count();
                let cancelled = rs.iter().filter(|r| r["validity"] == "INTERRUPTED").count();
                let total = rs.len();
                d["completed_runs"] = json!(good);
                d["failed_runs"] = json!(total - good - cancelled);
                d["cancelled_runs"] = json!(cancelled);
            }
            self.persist().await?;
            if p.global_settings.mode == "Step-by-step"
                && index + 1 < p.execution_order.len()
                && !self.cancelled().await
            {
                self.state("WAITING_FOR_USER").await;
                loop {
                    let mut d = self.data.lock().await;
                    if d["continue_requested"] == true || d["cancel_requested"] == true {
                        d["continue_requested"] = false.into();
                        break;
                    }
                    drop(d);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        self.state(if self.cancelled().await {
            "CANCELLED"
        } else {
            "COMPLETED"
        })
        .await;
        Ok(())
    }
    async fn window(
        &self,
        core: &Core,
        p: &Plan,
        c: &mic_protocol::lab::TransportConfig,
        start: &Value,
        seconds: u64,
    ) -> Result<(), Value> {
        let watchdog = std::time::Instant::now();
        let initial = counters(start, &start["diagnostics"]);
        loop {
            if self.cancelled().await {
                return Err(json!({"code":"USER_CANCELLED"}));
            }
            let obs = Self::observation(core).await;
            validate_observation(start, &obs, &p.device_id, c)?;
            delta(&initial, &counters(&obs, &obs["diagnostics"]))
                .map_err(|e| json!({"code":"COUNTER_RESET","detail":e}))?;
            let elapsed = obs["counter_monotonic_us"]
                .as_u64()
                .unwrap()
                .saturating_sub(start["counter_monotonic_us"].as_u64().unwrap());
            let future = p
                .execution_order
                .iter()
                .skip(self.data.lock().await["runs"].as_array().unwrap().len() + 1)
                .map(|(id, _)| {
                    p.selected_combinations
                        .iter()
                        .find(|c| &c.combination_id == id)
                        .map(|c| {
                            c.duration_seconds
                                .unwrap_or(p.global_settings.duration_seconds)
                                + p.global_settings.warmup_seconds
                        })
                        .unwrap_or(0)
                })
                .sum::<u64>();
            self.data.lock().await["estimated_remaining_seconds"] =
                json!(future as f64 + (seconds as f64 - elapsed as f64 / 1e6).max(0.));
            self.data.lock().await["remaining_seconds"] =
                json!((seconds as f64 - elapsed as f64 / 1e6).max(0.));
            if elapsed >= seconds * 1_000_000 {
                return Ok(());
            }
            if watchdog.elapsed().as_secs() > seconds + 5 {
                return Err(json!({"code":"RX_CLOCK_STALLED"}));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
pub fn validate_observation(
    a: &Value,
    b: &Value,
    id: &str,
    c: &mic_protocol::lab::TransportConfig,
) -> Result<(), Value> {
    let code = if b["active_device"] != id
        || !b["devices"].as_array().is_some_and(|ds| {
            ds.iter()
                .any(|d| d["id"] == id && d["state"] == "STREAMING")
        }) {
        Some("TX_DISCONNECTED")
    } else if a["diagnostic_stream_id"] != b["diagnostic_stream_id"]
        || b["diagnostic_stream_id"] != b["diagnostics"]["current_stream_id"]
        || a["diagnostics"]["current_session_id"] != b["diagnostics"]["current_session_id"]
    {
        Some("STREAM_CHANGED")
    } else if b["lab"][id]["applied"] != json!(c) || b["lab"][id]["status"] != "applied" {
        Some("CONFIG_CHANGED")
    } else if a["diagnostics"]["clock_id"] != b["diagnostics"]["clock_id"]
        || b["counter_monotonic_us"].as_u64() < a["counter_monotonic_us"].as_u64()
    {
        Some("CLOCK_CHANGED")
    } else if b["diagnostics"]["enabled"] != true
        || a["diagnostics"]["enabled_changes"] != b["diagnostics"]["enabled_changes"]
        || a["diagnostics"]["drop_count"] != b["diagnostics"]["drop_count"]
    {
        Some("DIAGNOSTICS_INCOMPLETE")
    } else {
        None
    };
    code.map_or(Ok(()), |code| Err(json!({"code":code})))
}
#[cfg(test)]
mod tests {
    use super::*;
    use mic_protocol::lab::TransportConfig;
    fn obs() -> Value {
        json!({"devices":[{"id":"tx","state":"STREAMING"}],"active_device":"tx","lab":{"tx":{"applied":TransportConfig::default(),"status":"applied"}},"diagnostic_stream_id":1,"counter_monotonic_us":100,"diagnostics":{"current_stream_id":1,"current_session_id":"session","clock_id":"rx","enabled":true,"enabled_changes":0,"drop_count":0}})
    }
    #[test]
    fn validity_faults() {
        let a = obs();
        assert!(validate_observation(&a, &a, "tx", &TransportConfig::default()).is_ok());
        for (field, value, code) in [
            ("diagnostic_stream_id", json!(2), "STREAM_CHANGED"),
            ("active_device", Value::Null, "TX_DISCONNECTED"),
            ("counter_monotonic_us", json!(0), "CLOCK_CHANGED"),
        ] {
            let mut b = a.clone();
            b[field] = value;
            assert_eq!(
                validate_observation(&a, &b, "tx", &TransportConfig::default()).unwrap_err()["code"],
                code
            );
        }
        let mut b = a.clone();
        b["lab"]["tx"]["applied"]["frame_duration_ms"] = 5.into();
        assert_eq!(
            validate_observation(&a, &b, "tx", &TransportConfig::default()).unwrap_err()["code"],
            "CONFIG_CHANGED"
        );
        b = a.clone();
        b["diagnostics"]["enabled"] = false.into();
        assert!(validate_observation(&a, &b, "tx", &TransportConfig::default()).is_err());
    }
    #[tokio::test]
    async fn persistence_and_cancel() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../artifacts")
            .join(format!("mymic-sweep-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        let s = Sweep {
            data: Arc::new(Mutex::new(
                json!({"schema_version":2,"suite_id":"suite-123","status":"MEASURING","runs":[{"validity":"VALID"}]}),
            )),
            gate: Arc::new(Mutex::new(())),
            root: Arc::new(root.clone()),
        };
        assert!(s.active().await);
        s.data.lock().await["cancel_requested"] = true.into();
        assert!(s.cancelled().await);
        s.state("CANCELLED").await;
        s.persist().await.unwrap();
        let v: Value =
            serde_json::from_slice(&std::fs::read(root.join("suite.json")).unwrap()).unwrap();
        assert_eq!(v["runs"].as_array().unwrap().len(), 1);
        assert!(root.join("suite-123.json").exists());
        std::fs::remove_file(root.join("suite.json")).unwrap();
        std::fs::remove_file(root.join("suite-123.json")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
