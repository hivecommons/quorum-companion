//! Betty's listening chime: a short, soft, rising two-note chime played when she heard her name
//! and is about to answer (quorum's `say.tone`). The hive's replies say `tone: true` once; the
//! companion then plays it on this computer's default output, never into the call (the capture is
//! the meeting app's own audio, not this app's).
//!
//! The sound is the one quorum renders for Discord and the Vexa bot (quorum `src/tone.ts`): the
//! parameters below are copied from there, so Betty chimes alike on every platform. Pure and
//! deterministic: no I/O.

use std::f64::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant};

/// 24 kHz mono, as in quorum.
pub const TONE_RATE_HZ: u32 = 24_000;
/// The two notes, rising a perfect fourth: G5 then C6.
pub const TONE_NOTE_HZ: [f64; 2] = [783.99, 1046.5];
/// Each note's length; the chime is about 200 ms.
pub const TONE_NOTE_MS: u64 = 100;
/// Attack: a fast, smooth rise (no click at the start).
pub const TONE_ATTACK_MS: u64 = 8;
/// Release: the last of each note fades to zero (no click at the end).
pub const TONE_RELEASE_MS: u64 = 25;
/// How far each note dies away over its length.
pub const TONE_DECAY_RATIO: f64 = 0.12;
/// The loudest sample as a fraction of full scale: a quiet cue.
pub const TONE_PEAK: f64 = 0.18;
/// Hard ceiling on the loudest sample, however the above change.
pub const TONE_PEAK_CAP: f64 = 0.25;
/// Full scale of a signed 16-bit sample.
pub const INT16_MAX: f64 = 32767.0;
const MS_PER_S: f64 = 1000.0;

/// How long the chime plays.
pub fn duration() -> Duration {
    Duration::from_millis(TONE_NOTE_MS * TONE_NOTE_HZ.len() as u64)
}

fn samples_for(ms: u64) -> usize {
    js_round(f64::from(TONE_RATE_HZ) * ms as f64 / MS_PER_S) as usize
}

/// JavaScript's Math.round (halves round up), so the samples match quorum's exactly.
fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// The chime as 24 kHz mono signed 16-bit samples, the same as quorum's renderListeningTone.
pub fn render() -> Vec<i16> {
    let note = samples_for(TONE_NOTE_MS);
    let attack = samples_for(TONE_ATTACK_MS);
    let release = samples_for(TONE_RELEASE_MS);
    let peak = js_round(TONE_PEAK.min(TONE_PEAK_CAP) * INT16_MAX);
    let decay_per_sample = TONE_DECAY_RATIO.ln() / note as f64;
    let mut out = Vec::with_capacity(note * TONE_NOTE_HZ.len());
    for hz in TONE_NOTE_HZ {
        for i in 0..note {
            let x = i as f64;
            let rise = if i < attack {
                (FRAC_PI_2 * (x / attack as f64)).sin().powi(2)
            } else {
                1.0
            };
            let fall = if i >= note - release {
                (FRAC_PI_2 * ((note - i) as f64 / release as f64))
                    .sin()
                    .powi(2)
            } else {
                1.0
            };
            let level = rise * fall * (decay_per_sample * x).exp();
            let v = js_round(peak * level * (2.0 * PI * hz * x / f64::from(TONE_RATE_HZ)).sin());
            out.push(v as i16);
        }
    }
    out
}

/// Decides whether a chime the hive asked for is played: never when the person turned the chime
/// off, and never over a chime that is still playing (one chime, not two). The companion does
/// not speak, so there is no speech of its own to wait for.
#[derive(Default)]
pub struct ToneGate {
    playing_until: Option<Instant>,
}

impl ToneGate {
    /// Calls `play` and answers true when the chime should sound now; otherwise drops it.
    pub fn offer(&mut self, enabled: bool, now: Instant, play: impl FnOnce()) -> bool {
        if !enabled {
            return false;
        }
        if self.playing_until.is_some_and(|until| now < until) {
            return false;
        }
        self.playing_until = Some(now + duration());
        play();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chime_is_quorums_two_notes_200_ms_long() {
        let pcm = render();
        assert_eq!(pcm.len(), 4800);
        assert_eq!(duration(), Duration::from_millis(200));
        // Reference samples from quorum's renderListeningTone (src/tone.ts).
        for (i, want) in [
            (0, 0),
            (7, 19),
            (100, 2861),
            (1234, 1843),
            (2400, 0),
            (3600, 1820),
        ] {
            assert!(
                (i32::from(pcm[i]) - want).abs() <= 1,
                "sample {i} = {}, want {want}",
                pcm[i]
            );
        }
    }

    #[test]
    fn the_chime_is_quiet_and_cannot_click() {
        let pcm = render();
        let peak = pcm.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        let ceiling = (TONE_PEAK * INT16_MAX).round() as i32;
        assert!(peak <= ceiling, "peak {peak} over {ceiling}");
        assert!((4979..=4981).contains(&peak), "peak {peak}");
        // Each note starts and ends at (about) zero.
        for at in [0, 2399, 2400, 4799] {
            assert!(pcm[at].abs() <= 1, "sample {at} = {}", pcm[at]);
        }
    }

    #[test]
    fn a_chime_plays_once_and_not_over_itself() {
        let mut gate = ToneGate::default();
        let t0 = Instant::now();
        let mut played = 0;
        assert!(gate.offer(true, t0, || played += 1));
        // A second request while the first is still sounding is dropped.
        assert!(!gate.offer(true, t0 + Duration::from_millis(150), || played += 1));
        assert_eq!(played, 1);
        assert!(gate.offer(true, t0 + duration(), || played += 1));
        assert_eq!(played, 2);
    }

    #[test]
    fn a_muted_chime_never_plays() {
        let mut gate = ToneGate::default();
        let mut played = 0;
        assert!(!gate.offer(false, Instant::now(), || played += 1));
        assert_eq!(played, 0);
        // Muting does not hold the next one back.
        assert!(gate.offer(true, Instant::now(), || played += 1));
        assert_eq!(played, 1);
    }
}
