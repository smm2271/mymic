use mic_protocol::Source;
use serde::Serialize;
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    Offline,
    Discovered,
    Pairing,
    Connected,
    Streaming,
}
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub capabilities: Vec<String>,
    pub address: String,
    pub state: State,
    pub source: Source,
    pub gain: f32,
    pub mute: bool,
    pub last_error: Option<String>,
}
impl Device {
    pub fn transition(&mut self, next: State) -> Result<(), &'static str> {
        let allowed = matches!(
            (self.state, next),
            (State::Offline, State::Discovered)
                | (State::Discovered, State::Pairing)
                | (State::Pairing, State::Connected)
                | (State::Pairing, State::Discovered)
                | (State::Connected, State::Streaming)
                | (State::Streaming, State::Connected)
        ) || next == State::Offline
            || next == self.state;
        if !allowed {
            return Err("invalid device state transition");
        };
        self.state = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn states() {
        let mut d = Device {
            id: "x".into(),
            name: "x".into(),
            device_type: "sim".into(),
            capabilities: vec![],
            address: "".into(),
            state: State::Offline,
            source: Source::Sine,
            gain: 1.0,
            mute: false,
            last_error: None,
        };
        assert!(d.transition(State::Streaming).is_err());
        for s in [
            State::Discovered,
            State::Pairing,
            State::Connected,
            State::Streaming,
            State::Connected,
            State::Offline,
            State::Discovered,
        ] {
            d.transition(s).unwrap();
        }
    }
}
