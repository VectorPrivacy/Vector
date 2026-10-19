//! A voice Channel's member sends only while speaking (CORD-07 §8); the rest of the
//! time a muted frame keeps their liveness count moving at a fraction of the bytes.

use crate::FRAME_MS;

/// The level counted as speech, on the meters' -60 dBFS to 0 scale: the stage's
/// speaking ring lights at the same.
pub const SPEECH_LEVEL: f32 = 0.15;
/// How long sending goes on after speech stops, so word endings and short pauses stay whole.
pub const SPEECH_HANGOVER_MS: u32 = 400;

#[derive(Default)]
pub struct SpeechGate {
    hang: u32,
}

impl SpeechGate {
    /// Once per frame, with its level: whether it goes out as audio. The first frame at
    /// speech level opens the gate.
    pub fn open(&mut self, level: f32) -> bool {
        if level > SPEECH_LEVEL {
            self.hang = SPEECH_HANGOVER_MS / FRAME_MS;
        } else {
            self.hang = self.hang.saturating_sub(1);
        }
        self.hang > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_opens_the_gate_at_once_and_a_hangover_closes_it() {
        let mut gate = SpeechGate::default();
        assert!(!gate.open(0.0), "silence sends nothing");
        assert!(!gate.open(SPEECH_LEVEL), "the threshold itself is not speech");
        assert!(gate.open(0.4), "the first loud frame goes out");
        for _ in 1..SPEECH_HANGOVER_MS / FRAME_MS {
            assert!(gate.open(0.0), "a pause shorter than the hangover keeps sending");
        }
        assert!(!gate.open(0.0), "the hangover ends");
        assert!(gate.open(0.3), "speech again reopens it");
    }
}
