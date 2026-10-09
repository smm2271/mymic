//! Optional authenticated Simulator-only diagnostics, independent of PCM wire format.
use crate::{tag, verify};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TxTiming {
    pub generated_packets: u64,
    pub successfully_sent_packets: u64,
    pub send_errors: u64,
    pub generated_samples: u64,
    pub missed_deadlines: u64,
    pub sequence_gaps_generated_by_tx: u64,
    pub intentional_loss_drops: u64,
    pub queue_drops: u64,
    pub packets_per_second: f64,
    pub samples_per_second: f64,
    pub scheduling_drift_ms: f64,
    pub scheduling_delay_p99_ms: Option<f64>,
    pub timing_mode: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub version: u8,
    pub kind: String,
    pub device_id: String,
    pub session_id: String,
    pub sequence: u64,
    pub timing: TxTiming,
    pub proof: String,
}
impl Diagnostic {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&(
            self.version,
            &self.kind,
            &self.device_id,
            &self.session_id,
            self.sequence,
            &self.timing,
        ))
        .unwrap()
    }
    pub fn sign(&mut self, key: &[u8]) {
        self.proof = hex::encode(tag(key, &self.bytes()));
    }
    pub fn verify(&self, key: &[u8]) -> bool {
        self.version == 1
            && self.kind == "TX_TIMING_STATS"
            && self.device_id.len() <= 64
            && self.session_id.len() == 32
            && self.timing.timing_mode.len() <= 32
            && hex::decode(&self.proof)
                .ok()
                .is_some_and(|p| verify(key, &self.bytes(), &p))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_are_authenticated() {
        let mut d = Diagnostic {
            version: 1,
            kind: "TX_TIMING_STATS".into(),
            device_id: "sim".into(),
            session_id: "0".repeat(32),
            sequence: 1,
            timing: TxTiming::default(),
            proof: String::new(),
        };
        d.sign(b"key");
        assert!(d.verify(b"key"));
        d.timing.generated_packets = 10;
        assert!(!d.verify(b"key"));
    }
}
