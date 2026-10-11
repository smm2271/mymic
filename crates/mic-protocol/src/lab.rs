//! Negotiated lab packets. Legacy v1 stays byte-for-byte unchanged.
use crate::{AudioPacket, tag};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
pub const CAPABILITY: &str = "audio_lab_v1";
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FecConfig {
    pub enabled: bool,
    pub scheme: Scheme,
    pub data_shards: u8,
    pub parity_shards: u8,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Xor,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    pub frame_duration_ms: u16,
    pub fec: FecConfig,
    pub jitter_buffer_target_ms: u16,
}
impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            frame_duration_ms: 10,
            fec: FecConfig {
                enabled: false,
                scheme: Scheme::Xor,
                data_shards: 4,
                parity_shards: 1,
            },
            jitter_buffer_target_ms: 30,
        }
    }
}
impl TransportConfig {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.frame_duration_ms, 5 | 10)
            || !matches!(
                self.jitter_buffer_target_ms,
                20 | 30 | 40 | 50 | 60 | 80 | 100
            )
            || self.fec.data_shards != 4
            || self.fec.parity_shards != 1
        {
            bail!("unsupported transport configuration")
        }
        Ok(())
    }
    pub fn samples(&self) -> usize {
        self.frame_duration_ms as usize * 16
    }
    pub fn depth(&self) -> usize {
        self.jitter_buffer_target_ms as usize / self.frame_duration_ms as usize
    }
}
#[derive(Clone, Debug)]
pub struct LabPacket {
    pub audio: AudioPacket,
    pub parity: bool,
    pub stream: u32,
    pub samples: usize,
}
impl LabPacket {
    pub fn encode(&self, key: &[u8]) -> Vec<u8> {
        assert!(matches!(self.samples, 80 | 160));
        let mut b = Vec::with_capacity(52 + 2 * self.samples);
        b.extend_from_slice(b"WMIC");
        b.extend([2, if self.parity { 2 } else { 1 }]);
        b.extend(self.audio.sequence.to_le_bytes());
        b.extend(self.audio.timestamp.to_le_bytes());
        b.extend((2 * self.samples as u16).to_le_bytes());
        b.extend(self.audio.session);
        b.extend(self.stream.to_le_bytes());
        for s in &self.audio.samples[..self.samples] {
            b.extend(s.to_le_bytes());
        }
        let t = tag(key, &b);
        b.extend(&t[..16]);
        b
    }
    pub fn decode(b: &[u8], key: &[u8], cfg: TransportConfig, stream: u32) -> Result<Self> {
        cfg.validate()?;
        let samples = cfg.samples();
        let n = 52 + 2 * samples;
        if b.len() != n
            || &b[..4] != b"WMIC"
            || b[4] != 2
            || !matches!(b[5], 1 | 2)
            || u16::from_le_bytes([b[14], b[15]]) as usize != 2 * samples
            || u32::from_le_bytes(b[32..36].try_into()?) != stream
            || (b[5] == 2 && !cfg.fec.enabled)
        {
            bail!("invalid lab packet or stream")
        }
        let t = tag(key, &b[..n - 16]);
        if b[n - 16..].iter().zip(t).fold(0u8, |a, (x, y)| a | (x ^ y)) != 0 {
            bail!("audio authentication failed")
        }
        let sequence = u32::from_le_bytes(b[6..10].try_into()?);
        if b[5] == 2 && sequence % 4 != 0 {
            bail!("unaligned parity group")
        }
        let mut pcm = [0; 160];
        for (i, s) in pcm[..samples].iter_mut().enumerate() {
            *s = i16::from_le_bytes([b[36 + 2 * i], b[37 + 2 * i]]);
        }
        Ok(Self {
            audio: AudioPacket {
                sequence,
                timestamp: u32::from_le_bytes(b[10..14].try_into()?),
                session: b[16..32].try_into()?,
                samples: pcm,
            },
            parity: b[5] == 2,
            stream,
            samples,
        })
    }
}
/// TX accumulator includes data even when its network send fails.
#[derive(Default)]
pub struct ParityEncoder {
    group: Option<LabPacket>,
    mask: u8,
}
impl ParityEncoder {
    pub fn push(&mut self, p: &LabPacket) -> Option<LabPacket> {
        let base = p.audio.sequence & !3;
        if self.group.as_ref().is_none_or(|g| {
            g.audio.sequence != base
                || g.stream != p.stream
                || g.audio.session != p.audio.session
                || g.samples != p.samples
        }) {
            let mut g = p.clone();
            g.parity = true;
            g.audio.sequence = base;
            g.audio.timestamp = p
                .audio
                .timestamp
                .wrapping_sub((p.audio.sequence - base) * p.samples as u32);
            g.audio.samples = [0; 160];
            self.group = Some(g);
            self.mask = 0;
        }
        let bit = 1 << (p.audio.sequence - base);
        if self.mask & bit != 0 {
            return None;
        }
        self.mask |= bit;
        let g = self.group.as_mut().unwrap();
        for (a, b) in g.audio.samples.iter_mut().zip(p.audio.samples) {
            *a ^= b;
        }
        if self.mask == 15 {
            let out = self.group.take();
            self.mask = 0;
            out
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_python_and_c_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/lab-vectors.json")).unwrap();
        let key = hex::decode(v["key_hex"].as_str().unwrap()).unwrap();
        for p in v["packets"].as_array().unwrap() {
            let mut cfg = TransportConfig {
                frame_duration_ms: if p["samples"] == 80 { 5 } else { 10 },
                ..TransportConfig::default()
            };
            cfg.fec.enabled = true;
            let b = hex::decode(p["packet_hex"].as_str().unwrap()).unwrap();
            let d = LabPacket::decode(&b, &key, cfg, 42).unwrap();
            assert_eq!(d.encode(&key), b);
            assert_eq!(d.audio.timestamp, p["timestamp"].as_u64().unwrap() as u32);
        }
        let mut m = crate::Message::new("SET_TRANSPORT", "golden-device", 42);
        m.session = v["session_hex"].as_str().unwrap().into();
        let mut cfg = TransportConfig {
            frame_duration_ms: 5,
            jitter_buffer_target_ms: 40,
            ..TransportConfig::default()
        };
        cfg.fec.enabled = true;
        m.transport = Some(cfg);
        m.transport_stream = Some(0);
        m.sign(&key);
        assert_eq!(
            m.signing_bytes(),
            v["control_canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(m.proof, v["control_proof"].as_str().unwrap());
    }
    #[test]
    fn lengths_authentication_stream_and_continuity() {
        for ms in [5, 10] {
            let cfg = TransportConfig {
                frame_duration_ms: ms,
                ..TransportConfig::default()
            };
            let mut generator = crate::Generator::default();
            for seq in 0..8u32 {
                let mut samples = [0; 160];
                if ms == 5 {
                    samples[..80].copy_from_slice(&generator.frame_n::<80>(1.0, false));
                } else {
                    samples = generator.frame(1.0, false);
                }
                let p = LabPacket {
                    audio: AudioPacket {
                        sequence: seq,
                        timestamp: seq * cfg.samples() as u32,
                        session: [1; 16],
                        samples,
                    },
                    parity: false,
                    stream: 42,
                    samples: cfg.samples(),
                };
                let mut b = p.encode(b"test");
                let d = LabPacket::decode(&b, b"test", cfg, 42).unwrap();
                assert_eq!(p.audio.samples, d.audio.samples);
                assert_eq!(generator.sample, (seq as u64 + 1) * cfg.samples() as u64);
                assert!(LabPacket::decode(&b, b"wrong", cfg, 42).is_err());
                assert!(LabPacket::decode(&b, b"test", cfg, 43).is_err());
                assert!(LabPacket::decode(&b[..10], b"test", cfg, 42).is_err());
                b[36] ^= 1;
                assert!(LabPacket::decode(&b, b"test", cfg, 42).is_err());
                let other = TransportConfig {
                    frame_duration_ms: if ms == 5 { 10 } else { 5 },
                    ..cfg
                };
                assert!(LabPacket::decode(&p.encode(b"test"), b"test", other, 42).is_err());
                assert!(AudioPacket::decode(&p.encode(b"test"), b"test").is_err());
            }
        }
    }
    #[test]
    fn settings_are_signed_and_validated_without_changing_legacy() {
        let mut m = crate::Message::new("SET_TRANSPORT", "tx", 1);
        m.transport = Some(TransportConfig::default());
        m.transport_stream = Some(44);
        m.sign(b"key");
        assert!(m.verify(b"key"));
        m.transport.as_mut().unwrap().fec.enabled = true;
        assert!(!m.verify(b"key"));
        m.sign(b"key");
        m.transport_stream = Some(45);
        assert!(!m.verify(b"key"));
        for ms in [0, 1, 6, 20] {
            assert!(
                TransportConfig {
                    frame_duration_ms: ms,
                    ..TransportConfig::default()
                }
                .validate()
                .is_err()
            );
        }
        for target in [0, 25, 200] {
            assert!(
                TransportConfig {
                    jitter_buffer_target_ms: target,
                    ..TransportConfig::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
