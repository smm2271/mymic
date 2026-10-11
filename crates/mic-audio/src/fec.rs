//! Authenticated input only. No waiting on parity and no separate playout path.
use mic_protocol::{AudioPacket, ahead, lab::LabPacket};
use serde::Serialize;
use std::collections::VecDeque;
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct FecMetrics {
    pub recovered: u64,
    pub unrecoverable: u64,
    pub too_late: u64,
    pub parity_received: u64,
    pub duplicates: u64,
    pub capacity_evictions: u64,
    pub notice_drops: u64,
}
struct Group {
    base: u32,
    timestamp: u32,
    session: [u8; 16],
    stream: u32,
    samples: usize,
    first_us: u64,
    data: [Option<AudioPacket>; 4],
    parity: Option<AudioPacket>,
    complete: bool,
}
#[derive(Default)]
pub struct FecDecoder {
    groups: VecDeque<Group>,
    pub metrics: FecMetrics,
    deadline: Option<u32>,
    notices: VecDeque<(crate::diagnostics::EventType, u32, &'static str)>,
}
impl FecDecoder {
    pub const CAPACITY: usize = 32;
    pub const TTL_US: u64 = 250_000;
    pub fn clear(&mut self) {
        self.groups.clear();
        self.deadline = None;
        self.notices.clear();
    }
    pub fn advance_deadline(&mut self, cursor: Option<u32>) {
        if let Some(c) = cursor
            && self.deadline.is_none_or(|d| ahead(c, d))
        {
            self.deadline = Some(c);
        }
    }
    pub fn take_notice(&mut self) -> Option<(crate::diagnostics::EventType, u32, &'static str)> {
        self.notices.pop_front()
    }
    pub fn deadline_sequence(&self) -> Option<u32> {
        self.deadline
    }
    fn notice(&mut self, kind: crate::diagnostics::EventType, seq: u32, reason: &'static str) {
        if self.notices.len() < 128 {
            self.notices.push_back((kind, seq, reason));
        } else {
            self.metrics.notice_drops += 1;
        }
    }
    pub fn occupancy(&self) -> usize {
        self.groups.len()
    }
    fn retire(&mut self, g: Group) {
        if !g.complete && g.data.iter().any(Option::is_none) {
            self.metrics.unrecoverable += 1;
            self.notice(
                crate::diagnostics::EventType::FecUnrecoverable,
                g.base,
                "group_expired_or_evicted_incomplete",
            );
        }
    }
    pub fn expire(&mut self, now: u64) {
        while self
            .groups
            .front()
            .is_some_and(|g| now.saturating_sub(g.first_us) >= Self::TTL_US)
        {
            let g = self.groups.pop_front().unwrap();
            self.retire(g);
        }
    }
    /// Packet must have passed wire HMAC, negotiated format and Session checks.
    pub fn receive(
        &mut self,
        p: &LabPacket,
        now: u64,
        expected: Option<u32>,
        started: bool,
    ) -> Option<AudioPacket> {
        self.expire(now);
        if p.parity {
            self.metrics.parity_received += 1;
            self.notice(
                crate::diagnostics::EventType::FecParityReceived,
                p.audio.sequence,
                "authenticated_parity",
            );
        }
        let base = p.audio.sequence & !3;
        let timestamp = p
            .audio
            .timestamp
            .wrapping_sub((p.audio.sequence - base) * p.samples as u32);
        let index =
            if let Some(i) = self.groups.iter().position(|g| {
                g.base == base && g.stream == p.stream && g.session == p.audio.session
            }) {
                i
            } else {
                if self.groups.len() == Self::CAPACITY {
                    let g = self.groups.pop_front().unwrap();
                    self.retire(g);
                    self.metrics.capacity_evictions += 1;
                }
                self.groups.push_back(Group {
                    base,
                    timestamp,
                    session: p.audio.session,
                    stream: p.stream,
                    samples: p.samples,
                    first_us: now,
                    data: std::array::from_fn(|_| None),
                    parity: None,
                    complete: false,
                });
                self.groups.len() - 1
            };
        let g = &mut self.groups[index];
        if g.samples != p.samples || g.timestamp != timestamp {
            return None;
        }
        let slot = if p.parity {
            &mut g.parity
        } else {
            &mut g.data[(p.audio.sequence - base) as usize]
        };
        if slot.is_some() {
            self.metrics.duplicates += 1;
            return None;
        }
        *slot = Some(p.audio.clone());
        if g.complete {
            return None;
        }
        let missing = g.data.iter().filter(|v| v.is_none()).count();
        if missing == 0 {
            g.complete = true;
            return None;
        }
        if missing != 1 || g.parity.is_none() {
            return None;
        }
        let i = g.data.iter().position(Option::is_none).unwrap();
        let mut recovered = g.parity.as_ref().unwrap().clone();
        recovered.sequence = base.wrapping_add(i as u32);
        recovered.timestamp = timestamp.wrapping_add(i as u32 * g.samples as u32);
        for d in g.data.iter().flatten() {
            for (a, b) in recovered.samples.iter_mut().zip(d.samples) {
                *a ^= b;
            }
        }
        g.data[i] = Some(recovered.clone());
        g.complete = true;
        if self.deadline.is_some_and(|e| ahead(e, recovered.sequence))
            || (started && expected.is_some_and(|e| ahead(e, recovered.sequence)))
        {
            self.metrics.too_late += 1;
            self.notice(
                crate::diagnostics::EventType::FecTooLate,
                recovered.sequence,
                "xor_recovery_behind_cursor",
            );
            return None;
        }
        self.metrics.recovered += 1;
        self.notice(
            crate::diagnostics::EventType::FecRecovered,
            recovered.sequence,
            "xor_single_missing_before_deadline",
        );
        Some(recovered)
    }
}
