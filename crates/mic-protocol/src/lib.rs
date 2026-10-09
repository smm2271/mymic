use anyhow::{Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
pub const VERSION: u8 = 1;
pub const SAMPLES: usize = 160;
pub const HEADER: usize = 32;
pub const PACKET_SIZE: usize = HEADER + SAMPLES * 2 + 16;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Sine,
    Sweep,
    Pulse,
    Silence,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub version: u8,
    pub kind: String,
    pub request_id: u32,
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub device_type: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub nonce: String,
    #[serde(default)]
    pub commitment: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub proof: String,
    #[serde(default)]
    pub audio_port: u16,
    #[serde(default)]
    pub control_port: u16,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub gain: Option<f32>,
    #[serde(default)]
    pub mute: Option<bool>,
    #[serde(default)]
    pub error: String,
}
impl Message {
    pub fn new(kind: &str, id: &str, request_id: u32) -> Self {
        Self {
            version: VERSION,
            kind: kind.into(),
            request_id,
            device_id: id.into(),
            name: String::new(),
            device_type: String::new(),
            capabilities: vec![],
            nonce: String::new(),
            commitment: String::new(),
            session: String::new(),
            proof: String::new(),
            audio_port: 0,
            control_port: 0,
            source: None,
            gain: None,
            mute: None,
            error: String::new(),
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 2048 {
            bail!("control message too large")
        }
        let m: Self = serde_json::from_slice(bytes)?;
        if m.version != VERSION
            || m.device_id.is_empty()
            || m.device_id.len() > 64
            || m.name.len() > 96
            || m.nonce.len() > 64
            || m.session.len() > 32
        {
            bail!("invalid control envelope")
        }
        if let Some(g) = m.gain
            && (!g.is_finite() || !(0.0..=1.0).contains(&g))
        {
            bail!("invalid gain")
        }
        Ok(m)
    }
    // Signing bytes are independent of JSON field order, float formatting and whitespace.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let source = self
            .source
            .as_ref()
            .map(|s| match s {
                Source::Sine => "sine",
                Source::Sweep => "sweep",
                Source::Pulse => "pulse",
                Source::Silence => "silence",
            })
            .unwrap_or("");
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.version,
            self.kind,
            self.request_id,
            self.device_id,
            self.nonce,
            self.session,
            self.audio_port,
            self.control_port,
            source,
            self.gain
                .map(|g| (g * 1000.0).round() as u32)
                .map(|g| g.to_string())
                .unwrap_or_default(),
            self.mute.map(|m| if m { "1" } else { "0" }).unwrap_or("")
        )
        .into_bytes()
    }
    pub fn sign(&mut self, key: &[u8]) {
        self.proof = hex::encode(tag(key, &self.signing_bytes()));
    }
    pub fn verify(&self, key: &[u8]) -> bool {
        hex::decode(&self.proof)
            .ok()
            .is_some_and(|p| verify(key, &self.signing_bytes(), &p))
    }
}
pub fn tag(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut h = Hmac::<Sha256>::new_from_slice(key).unwrap();
    h.update(data);
    h.finalize().into_bytes().into()
}
pub fn verify(key: &[u8], data: &[u8], proof: &[u8]) -> bool {
    let mut h = Hmac::<Sha256>::new_from_slice(key).unwrap();
    h.update(data);
    h.verify_slice(proof).is_ok()
}
pub fn commitment(code: &str, nonce: &str) -> String {
    use sha2::Digest;
    hex::encode(Sha256::digest(format!("{code}|{nonce}")))
}
pub fn session_key(code: &str, nonce: &str, session: &str) -> [u8; 32] {
    tag(code.as_bytes(), format!("{nonce}|{session}").as_bytes())
}
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub sequence: u32,
    pub timestamp: u32,
    pub session: [u8; 16],
    pub samples: [i16; SAMPLES],
}
impl AudioPacket {
    pub fn encode(&self, key: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(PACKET_SIZE);
        b.extend_from_slice(b"WMIC");
        b.extend([VERSION, 1]);
        b.extend(self.sequence.to_le_bytes());
        b.extend(self.timestamp.to_le_bytes());
        b.extend((SAMPLES as u16 * 2).to_le_bytes());
        b.extend(self.session);
        for x in self.samples {
            b.extend(x.to_le_bytes());
        }
        let t = tag(key, &b);
        b.extend(&t[..16]);
        b
    }
    pub fn decode(b: &[u8], key: &[u8]) -> Result<Self> {
        if b.len() != PACKET_SIZE
            || &b[..4] != b"WMIC"
            || b[4] != VERSION
            || b[5] != 1
            || u16::from_le_bytes([b[14], b[15]]) != 320
        {
            bail!("invalid audio packet")
        }
        // Constant time verification of the truncated tag.
        let t = tag(key, &b[..PACKET_SIZE - 16]);
        let difference = b[PACKET_SIZE - 16..]
            .iter()
            .zip(t)
            .fold(0u8, |a, (x, y)| a | (x ^ y));
        if difference != 0 {
            bail!("audio authentication failed")
        }
        let mut samples = [0; SAMPLES];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = i16::from_le_bytes([b[HEADER + i * 2], b[HEADER + i * 2 + 1]]);
        }
        Ok(Self {
            sequence: u32::from_le_bytes(b[6..10].try_into()?),
            timestamp: u32::from_le_bytes(b[10..14].try_into()?),
            session: b[16..32].try_into()?,
            samples,
        })
    }
}
pub fn ahead(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}
pub struct Generator {
    pub source: Source,
    pub sample: u64,
    pub phase: f64,
}
impl Default for Generator {
    fn default() -> Self {
        Self {
            source: Source::Sine,
            sample: 0,
            phase: 0.0,
        }
    }
}
impl Generator {
    pub fn frame(&mut self, gain: f32, mute: bool) -> [i16; SAMPLES] {
        std::array::from_fn(|_| {
            let t = self.sample as f64 / 16000.0;
            let freq = match self.source {
                Source::Sweep => 200.0 + 1800.0 * (t % 5.0) / 5.0,
                _ => 440.0,
            };
            self.phase = (self.phase + freq / 16000.0) % 1.0;
            let value = match self.source {
                Source::Sine | Source::Sweep => (self.phase * std::f64::consts::TAU).sin(),
                Source::Pulse => {
                    if t % 1.0 < 0.02 {
                        0.5
                    } else {
                        0.0
                    }
                }
                Source::Silence => 0.0,
            };
            self.sample += 1;
            if mute {
                0
            } else {
                (value * 32767.0 * 0.15 * gain.clamp(0.0, 1.0) as f64) as i16
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_roundtrip_and_invalid() {
        let p = AudioPacket {
            sequence: u32::MAX,
            timestamp: 160,
            session: [1; 16],
            samples: [-123; 160],
        };
        let mut b = p.encode(b"key");
        assert_eq!(AudioPacket::decode(&b, b"key").unwrap().samples, p.samples);
        b[32] ^= 1;
        assert!(AudioPacket::decode(&b, b"key").is_err());
        assert!(AudioPacket::decode(&b[..16], b"key").is_err());
    }
    #[test]
    fn sequence_wrap() {
        assert!(ahead(0, u32::MAX));
        assert!(!ahead(u32::MAX, 0));
    }
    #[test]
    fn control_roundtrip() {
        let mut m = Message::new("START_STREAM", "test", 1);
        m.sign(b"secret");
        let n = Message::decode(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(n.verify(b"secret"));
        assert!(!n.verify(b"other"));
    }
    #[test]
    fn switching_source() {
        let mut g = Generator::default();
        assert!(g.frame(1.0, false).iter().any(|x| *x != 0));
        g.source = Source::Silence;
        assert_eq!(g.frame(1.0, false), [0; 160]);
        g.source = Source::Pulse;
        assert_eq!(g.frame(1.0, true), [0; 160]);
    }
}

/// Pairs the Windows 1 ms timer request with release when the process stops.
pub struct TimerResolution;
impl TimerResolution {
    pub fn acquire() -> Self {
        #[cfg(windows)]
        // SAFETY: process-scoped timer request with a valid period, released in Drop.
        unsafe {
            windows_sys::Win32::Media::timeBeginPeriod(1);
        }
        Self
    }
}
impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: balances the request in acquire.
        unsafe {
            windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
}

#[cfg(test)]
mod golden {
    use super::*;
    #[test]
    fn shared_c_rust_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/vectors.json")).unwrap();
        let key = hex::decode(v["key_hex"].as_str().unwrap()).unwrap();
        let bytes = hex::decode(v["packet_hex"].as_str().unwrap()).unwrap();
        let p = AudioPacket::decode(&bytes, &key).unwrap();
        assert_eq!(p.sequence, u32::MAX);
        assert_eq!(p.timestamp, 160);
        assert_eq!(&p.samples[..5], &[-32768, -1, 0, 1, 32767]);
        assert_eq!(p.encode(&key), bytes);
        let mut m = Message::new("SET_GAIN", "golden-device", u32::MAX);
        m.session = v["session_hex"].as_str().unwrap().into();
        m.gain = Some(0.25);
        m.sign(&key);
        assert_eq!(
            m.signing_bytes(),
            v["control_canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(m.proof, v["control_proof"].as_str().unwrap());
    }
}
