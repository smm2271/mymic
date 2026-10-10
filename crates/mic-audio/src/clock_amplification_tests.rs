//! A.5 test-only observer of the existing Stage A multi-clock execution.
use crate::{Jitter, diagnostics::Diagnostics};
use serde::Serialize;
use std::{collections::VecDeque, sync::atomic::Ordering};

#[derive(Clone, Debug, Serialize)]
pub(super) struct State {
    expected: Option<u32>,
    window: Vec<u32>,
    occupancy: usize,
    contiguous: usize,
    epoch: u64,
    started: bool,
    received: u64,
    admitted: u64,
    delivered: u64,
    missing: u64,
    capacity_and_forward_drops: u64,
    overruns: u64,
    late: u64,
    duplicates: u64,
    underruns: u64,
}
impl State {
    pub fn read(j: &Jitter, d: &Diagnostics) -> Self {
        let mut window: Vec<_> = j.packets.keys().copied().collect();
        window.sort_by_key(|s| s.wrapping_sub(j.expected.unwrap_or(0)));
        Self {
            expected: j.expected,
            window,
            occupancy: j.occupancy(),
            contiguous: j.contiguous_playable_depth(),
            epoch: d.playout_epoch.load(Ordering::Relaxed),
            started: j.started,
            received: j.metrics.received_packets,
            admitted: j.metrics.accepted_packets,
            delivered: j.metrics.delivered_packets,
            missing: j.metrics.lost_packets,
            capacity_and_forward_drops: j.metrics.jitter_buffer_drops,
            overruns: j.metrics.buffer_overruns,
            late: j.metrics.late_packet_drops,
            duplicates: j.metrics.duplicate_packets,
            underruns: j.metrics.buffer_underruns,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
struct Step {
    virtual_ns: f64,
    operation: &'static str,
    packet: Option<(u32, u32)>,
    slot: Option<u32>,
    outcome: &'static str,
    before: State,
    after: State,
    reasons: Vec<&'static str>,
}
#[derive(Default, Serialize)]
pub(super) struct Investigation {
    pub first_drop_ns: Option<f64>,
    pub switch_to: Option<i32>,
    pub switch_ns: Option<f64>,
    pub tie_tick_first: bool,
    final_state: Option<State>,
    first_missing_ns: Option<f64>,
    first_drop: Option<Step>,
    #[serde(skip)]
    history: VecDeque<Step>,
    timeline: Vec<Step>,
    reasons: std::collections::BTreeMap<&'static str, u64>,
    max_drop_in_one_arrival: u64,
    capacity_decisions: u64,
    forward_decisions: u64,
    min_occupancy_after_fault: Option<usize>,
    max_occupancy_after_fault: usize,
    epoch_transitions: u64,
    cursor_rewinds: u64,
    #[serde(skip)]
    last_slot: Option<(u64, u32)>,
    #[serde(skip)]
    last_epoch: Option<u64>,
}
impl Investigation {
    pub fn observe(
        &mut self,
        now: f64,
        operation: &'static str,
        packet: Option<(u32, u32)>,
        before: State,
        after: State,
        d: &Diagnostics,
    ) {
        let mut reasons = Vec::new();
        while let Some(e) = d.queue.pop() {
            *self.reasons.entry(e.reason).or_default() += 1;
            reasons.push(e.reason);
        }
        let dropped = after.capacity_and_forward_drops - before.capacity_and_forward_drops;
        if operation == "arrival" {
            assert_eq!(after.received - before.received, 1);
            assert!(dropped <= 1);
            self.max_drop_in_one_arrival = self.max_drop_in_one_arrival.max(dropped);
            self.capacity_decisions += after.overruns - before.overruns;
            self.forward_decisions += dropped - (after.overruns - before.overruns);
        }
        if self.last_epoch.is_some_and(|e| e != after.epoch) {
            self.epoch_transitions += 1;
        }
        self.last_epoch = Some(after.epoch);
        let slot = if operation == "playout"
            && (after.delivered > before.delivered || after.missing > before.missing)
        {
            before.expected
        } else {
            None
        };
        if let Some(seq) = slot {
            if let Some((epoch, last)) = self.last_slot {
                if epoch == before.epoch {
                    assert_eq!(
                        seq,
                        last.wrapping_add(1),
                        "same-epoch slot is not monotonic"
                    );
                }
                if mic_protocol::ahead(last, seq) {
                    self.cursor_rewinds += 1;
                }
            }
            self.last_slot = Some((before.epoch, seq));
        }
        let outcome = if after.delivered > before.delivered {
            "played"
        } else if after.missing > before.missing {
            "concealed_zero_slot"
        } else if dropped > 0 {
            "discarded_packet"
        } else if after.admitted > before.admitted {
            "admitted"
        } else {
            "no_slot_decision"
        };
        if after.missing > before.missing && self.first_missing_ns.is_none() {
            self.first_missing_ns = Some(now);
        }
        self.final_state = Some(after.clone());
        let step = Step {
            virtual_ns: now,
            operation,
            packet,
            slot,
            outcome,
            before,
            after: after.clone(),
            reasons,
        };
        if after.overruns > step.before.overruns && self.first_drop_ns.is_none() {
            self.first_drop_ns = Some(now);
            self.first_drop = Some(step.clone());
            self.timeline.extend(self.history.iter().cloned());
            self.history.clear();
        }
        if let Some(first) = self.first_drop_ns {
            self.min_occupancy_after_fault = Some(
                self.min_occupancy_after_fault
                    .unwrap_or(after.occupancy)
                    .min(after.occupancy),
            );
            self.max_occupancy_after_fault = self.max_occupancy_after_fault.max(after.occupancy);
            if now <= first + 12e9 {
                self.timeline.push(step);
            }
        } else {
            self.history.push_back(step);
            while self
                .history
                .front()
                .is_some_and(|s| s.virtual_ns < now - 12e9)
            {
                self.history.pop_front();
            }
        }
        assert!(self.history.len() <= 5000 && self.timeline.len() <= 10000);
    }
}

#[test]
#[ignore = "A.5 actual 30/40-minute virtual-clock investigation; run explicitly"]
fn amplification_and_rate_recovery_actual_implementations() {
    let dir = std::env::var_os("MIC_DRIFT_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .expect("set MIC_DRIFT_ARTIFACT_DIR to preserve evidence");
    std::fs::create_dir_all(&dir).unwrap();
    let mut reports = Vec::new();
    for (name, initial, switch_to, seconds, tie_tick_first) in [
        ("control", 0, None, 1800, false),
        ("baseline", 50, None, 1800, false),
        ("recover_zero", 50, Some(0), 2400, false),
        ("recover_negative", 50, Some(-50), 2400, false),
        ("recover_zero_tick_first", 50, Some(0), 2400, true),
    ] {
        let mut observer = Investigation {
            switch_to,
            tie_tick_first,
            ..Investigation::default()
        };
        let on = super::clock_drift_tests::run_profile(
            initial,
            0,
            true,
            false,
            seconds,
            Some(&mut observer),
        );
        let mut off_observer = Investigation {
            switch_to,
            tie_tick_first,
            ..Investigation::default()
        };
        let off = super::clock_drift_tests::run_profile(
            initial,
            0,
            false,
            false,
            seconds,
            Some(&mut off_observer),
        );
        assert_eq!(on, off, "diagnostics changed audio for {name}");
        assert_eq!(observer.first_drop_ns, off_observer.first_drop_ns);
        if initial == 0 {
            assert_eq!(on.missing, 0);
            assert_eq!(on.capacity_drops, 0);
        } else {
            assert!(observer.first_drop_ns.is_some());
        }
        let tail: &[_] = &on.levels[on.levels.len().saturating_sub(61)..];
        let tail_missing = tail.last().unwrap().3 - tail.first().unwrap().3;
        let tail_drops = tail.last().unwrap().4 - tail.first().unwrap().4;
        let report = serde_json::json!({"scenario":name,"initial_ppm":initial,"switch_ppm":switch_to,"virtual_seconds":seconds,"checksum":on.checksum,"on_off_equal":true,"missing_total":on.missing,"drops_total":on.capacity_drops,"last_60s_missing":tail_missing,"last_60s_drops":tail_drops,"tail_stable":tail_missing==0&&tail_drops==0,"investigation":observer});
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("{name}-levels.json")),
            serde_json::to_vec(&on.levels).unwrap(),
        )
        .unwrap();
        reports.push(serde_json::json!({"scenario":name,"virtual_seconds":seconds,"on_off_equal":true,"checksum":on.checksum,"missing":on.missing,"drops":on.capacity_drops,"tail_missing":tail_missing,"tail_drops":tail_drops}));
    }
    std::fs::write(
        dir.join("summary.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
}

#[test]
fn capacity_missing_counter_semantics_and_epoch_wrap() {
    let d = Diagnostics::bounded(128);
    let mut j = Jitter::new(2);
    j.diagnostics = Some(d.clone());
    let p = |n: u32| mic_protocol::AudioPacket {
        sequence: n,
        timestamp: n.wrapping_mul(160),
        session: [0; 16],
        samples: [123; 160],
    };
    for n in 0..8 {
        j.push(p(n));
    }
    let expected = j.expected;
    j.push(p(8));
    assert_eq!(j.metrics.jitter_buffer_drops, 1);
    assert_eq!(j.metrics.buffer_overruns, 1);
    assert_eq!(j.expected, expected);
    j.push(p(8));
    assert_eq!(j.metrics.jitter_buffer_drops, 2);
    assert_eq!(
        j.metrics.duplicate_packets, 0,
        "rejected packet not in seen, retry is another decision"
    );
    for _ in 0..8 {
        j.tick();
    }
    j.tick();
    assert_eq!(j.metrics.lost_packets, 1);
    assert_eq!(j.metrics.buffer_underruns, 1);
    let before = j.metrics.jitter_buffer_drops;
    j.push(p(8));
    assert_eq!(j.metrics.late_packet_drops, 1);
    assert_eq!(j.metrics.jitter_buffer_drops, before);
    j.muted = true;
    j.tick();
    assert_eq!(j.metrics.lost_packets, 2);
    assert_eq!(j.metrics.buffer_underruns, 1);
    j.reset();
    let epoch = d.playout_epoch.load(Ordering::Relaxed);
    j.push(p(u32::MAX));
    j.push(p(0));
    assert_eq!(j.tick()[0], 123);
    assert_eq!(j.expected, Some(0));
    assert_eq!(j.tick()[0], 123);
    assert_eq!(j.expected, Some(1));
    assert!(d.playout_epoch.load(Ordering::Relaxed) > epoch);
    assert!(
        d.drain()
            .iter()
            .any(|r| r.event.reason == "jitter_capacity_reject_farther")
    );
    assert_eq!(
        d.snapshot().counts[crate::diagnostics::EventType::BufferResync as usize],
        2,
        "capacity events did not reset the cursor but each emitted one envelope"
    );
}

#[test]
#[ignore = "known unmet future recovery contract; preserves current failure for Stage C review"]
fn zero_rate_recovery_contract_after_one_capacity_drop() {
    let mut j = Jitter::new(3);
    let p = |n: u32| mic_protocol::AudioPacket {
        sequence: n,
        timestamp: n * 160,
        session: [0; 16],
        samples: [123; 160],
    };
    for n in 0..8 {
        j.push(p(n));
    }
    j.tick();
    j.push(p(8));
    let mut at_90 = 0;
    for deadline in 0..180 {
        // One unique packet per one virtual 10 ms deadline, arrival first.
        j.push(p(9 + deadline));
        j.tick();
        if deadline == 89 {
            at_90 = j.metrics.lost_packets;
        }
    }
    let tail_missing = j.metrics.lost_packets - at_90;
    if let Some(dir) = std::env::var_os("MIC_DRIFT_ARTIFACT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::PathBuf::from(dir).join("known-unmet-zero-recovery.json"),serde_json::to_vec_pretty(&serde_json::json!({"virtual_deadlines":180,"unique_arrivals":180,"precondition_received_packets":9,"precondition_delivered_packets":1,"precondition_buffer_packets":8,"target_prefill_packets":3,"missing_last_90_deadlines":tail_missing,"capacity_drops":j.metrics.jitter_buffer_drops,"expected":j.expected_sequence(),"desired_tail_missing":0})).unwrap()).unwrap();
    }
    assert_eq!(
        tail_missing, 0,
        "known unmet stability contract: zero net rate does not clear the arrival-first capacity/missing cycle"
    );
}
