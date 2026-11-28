//! Sharp pictures on a narrow link (the user, 2026-11-26: dragging a window on a PC on a mobile network was very
//! blurry, and the picture took seconds to sharpen after the drag).
//!
//! Measured then: the PC's LTE upload carried 5.7 Mbit/s, the session sent 0.5 to 2.9. str0m's estimate (Google's
//! congestion control) cannot rise far above what was sent: on a still screen, which sends almost nothing, one delay
//! spike (a keyframe's burst on the mobile link) brings it down to a fraction of what the link carries, and it then
//! climbs back only as fast as the encoder's output grows. A drag after a pause started with under 1 Mbit/s.
//!
//! [`Hold`]: a second in which the screen changed and the encoder used most of its bitrate says what the link carries
//! (the estimate then, under load); a still second says nothing and changes nothing. When the screen changes again
//! after a pause and the estimate is far under what was carried, the estimate starts again from it
//! (`Peer::reset_bitrate`). A link that no longer carries it brings it down within a second or so, and the next busy
//! second holds that instead.
//!
//! [`Refresh`]: after motion, once the screen has been still for a moment, the last picture is encoded again as a
//! keyframe: a fresh picture from no blurry reference, instead of sharpening a little with each small frame.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// What the encoder sent lately (changed frames, keyframes left out) against its bitrate: motion is when it uses
/// most of it (a drag on a narrow link used 60 to 95 %), the screen is still when it uses little. Not "nothing
/// changed": most screens have a clock, a caret or an animation somewhere (the test laptop's animated wallpaper kept
/// sending 15 frames a second, a quarter of the bitrate, 2026-11-26).
const WINDOW: Duration = Duration::from_millis(300);
const MOTION_SHARE: f64 = 0.5;
const STILL_SHARE: f64 = 0.35;

#[derive(Default)]
pub(crate) struct Motion {
    sent: VecDeque<(Instant, usize)>,
}

impl Motion {
    /// A changed frame of `bytes` went out (not a keyframe).
    pub(crate) fn record(&mut self, now: Instant, bytes: usize) {
        self.sent.push_back((now, bytes));
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        while self
            .sent
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > WINDOW)
        {
            self.sent.pop_front();
        }
    }

    fn rate(&mut self, now: Instant) -> f64 {
        self.prune(now);
        self.sent.iter().map(|(_, bytes)| *bytes).sum::<usize>() as f64 * 8.0 / WINDOW.as_secs_f64()
    }

    /// The last WINDOW used most of `bitrate`.
    pub(crate) fn moving(&mut self, now: Instant, bitrate: u32) -> bool {
        self.rate(now) >= MOTION_SHARE * f64::from(bitrate)
    }

    /// The last WINDOW used little of `bitrate`.
    pub(crate) fn still(&mut self, now: Instant, bitrate: u32) -> bool {
        self.rate(now) < STILL_SHARE * f64::from(bitrate)
    }
}

/// A second counts as busy when the encoder sent at least this share of its bitrate in it...
const BUSY_SHARE: f64 = 0.6;
/// ...and the screen changed in at least this many of its frames (a keyframe alone, sent again on request or as a
/// refresh, is no measure of the link).
const BUSY_FRAMES: u32 = 5;
/// The screen unchanged this long is a pause.
const PAUSE: Duration = Duration::from_secs(1);
/// An estimate under this share of what was carried starts again from it...
const FAR_UNDER: f64 = 0.7;
/// ...at most this often.
const RESET_GAP: Duration = Duration::from_secs(3);

/// The bitrate the link has carried, held while the screen is still.
pub(crate) struct Hold {
    /// The estimate in the last busy second; until there was one, the one every session starts with.
    carried: u64,
    /// The current second: when it began, the bytes sent and the changed frames by then.
    second: (Instant, usize, u32),
    changed_frames: u32,
    last_change: Option<Instant>,
    last_reset: Option<Instant>,
    pub(crate) resets: u32,
}

impl Hold {
    pub(crate) fn new(start: u64, now: Instant) -> Hold {
        Hold {
            carried: start,
            second: (now, 0, 0),
            changed_frames: 0,
            last_change: None,
            last_reset: None,
            resets: 0,
        }
    }

    /// What the link carried, as held now.
    pub(crate) fn carried(&self) -> u64 {
        self.carried
    }

    /// Called on every pass: once a second passed, a busy one takes `estimate` as what the link carries. `bytes`: all
    /// sent so far; `encoder_bitrate`: what the encoder was given.
    pub(crate) fn tick(&mut self, now: Instant, bytes: usize, encoder_bitrate: u32, estimate: u64) {
        let seconds = now.saturating_duration_since(self.second.0).as_secs_f64();
        if seconds < 1.0 {
            return;
        }
        let sent = bytes.saturating_sub(self.second.1) as f64 * 8.0 / seconds;
        let changed = self.changed_frames - self.second.2;
        if changed >= BUSY_FRAMES && sent >= BUSY_SHARE * f64::from(encoder_bitrate) {
            self.carried = estimate;
        }
        self.second = (now, bytes, self.changed_frames);
    }

    /// Motion was encoded ([`Motion::moving`]): the bitrate to start the estimate again from, when there was none for a
    /// while before and the estimate sank far under what was carried meanwhile.
    pub(crate) fn changed(&mut self, now: Instant, estimate: u64) -> Option<u64> {
        let paused = self
            .last_change
            .is_none_or(|at| now.saturating_duration_since(at) >= PAUSE);
        self.last_change = Some(now);
        self.changed_frames += 1;
        let far_under = (estimate as f64) < FAR_UNDER * self.carried as f64;
        let spaced = self
            .last_reset
            .is_none_or(|at| now.saturating_duration_since(at) >= RESET_GAP);
        if paused && far_under && spaced {
            self.last_reset = Some(now);
            self.resets += 1;
            Some(self.carried)
        } else {
            None
        }
    }
}

/// Changed frames since the last keyframe after which a pause earns a refresh (a drag, not a caret's blink)...
const REFRESH_MOTION: u32 = 10;
/// ...once the screen has been still this long.
const REFRESH_AFTER: Duration = Duration::from_millis(300);

/// A keyframe of the still screen after motion.
#[derive(Default)]
pub(crate) struct Refresh {
    motion: u32,
    last_change: Option<Instant>,
    pub(crate) count: u32,
}

impl Refresh {
    /// Motion was encoded ([`Motion::moving`]). Only a run of it counts, without a pause: motion now and then never adds
    /// up to a refresh.
    pub(crate) fn changed(&mut self, now: Instant) {
        if self
            .last_change
            .is_some_and(|at| now.saturating_duration_since(at) >= PAUSE)
        {
            self.motion = 0;
        }
        self.motion += 1;
        self.last_change = Some(now);
    }

    /// A keyframe went out (any: it is a fresh picture already).
    pub(crate) fn keyframe(&mut self) {
        self.motion = 0;
    }

    /// Whether to encode the last picture again as a keyframe now (counted when so): after a run of motion, none for
    /// REFRESH_AFTER and the screen `still` ([`Motion::still`]).
    pub(crate) fn due(&mut self, now: Instant, still: bool) -> bool {
        let still = still
            && self
                .last_change
                .is_some_and(|at| now.saturating_duration_since(at) >= REFRESH_AFTER);
        if self.motion >= REFRESH_MOTION && still {
            self.motion = 0;
            self.count += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBIT: u64 = 1_000_000;

    /// One second of `frames` changed frames sending `kbit` at an encoder bitrate of `encoder` Mbit/s, the estimate
    /// `estimate` Mbit/s at its end. Returns the time after it and the bytes sent by then.
    fn second(
        hold: &mut Hold,
        at: Instant,
        bytes: usize,
        frames: u32,
        kbit: usize,
        encoder: u32,
        estimate: f64,
    ) -> (Instant, usize) {
        for frame in 0..frames {
            let _ = hold.changed(
                at + Duration::from_millis(u64::from(frame) * 20),
                (estimate * MBIT as f64) as u64,
            );
        }
        let end = at + Duration::from_secs(1);
        let bytes = bytes + kbit * 1000 / 8;
        hold.tick(
            end,
            bytes,
            encoder * 1_000_000,
            (estimate * MBIT as f64) as u64,
        );
        (end, bytes)
    }

    #[test]
    fn motion_uses_most_of_the_bitrate_and_an_animated_wallpaper_does_not() {
        // As measured on the test laptop at 5.8 Mbit/s: its animated wallpaper, 15 frames a second of 11 kB, a quarter
        // of it: still, not motion.
        let start = Instant::now();
        let bitrate = 5_800_000;
        let mut wallpaper = Motion::default();
        for frame in 0..30 {
            wallpaper.record(start + Duration::from_millis(frame * 66), 11_000);
        }
        let end = start + Duration::from_millis(29 * 66);
        assert!(!wallpaper.moving(end, bitrate) && wallpaper.still(end, bitrate));
        // A drag, 40 frames a second of 12 kB (two thirds of it): motion; 300 ms after it, still again.
        let mut drag = Motion::default();
        for frame in 0..40 {
            drag.record(start + Duration::from_millis(frame * 25), 12_000);
        }
        let end = start + Duration::from_millis(39 * 25);
        assert!(drag.moving(end, bitrate));
        assert!(drag.still(end + Duration::from_millis(400), bitrate));
        // At 18 Mbit/s the same drag is not short of bits: no motion to sharpen after.
        assert!(!drag.moving(end, 18_000_000));
    }

    #[test]
    fn a_busy_second_sets_what_the_link_carries_and_a_still_one_does_not() {
        let start = Instant::now();
        let mut hold = Hold::new(8 * MBIT, start);
        // A drag: 40 changed frames, 3.6 Mbit sent of 4 allowed, the estimate 4.7: carried 4.7.
        let (at, bytes) = second(&mut hold, start, 0, 40, 3600, 4, 4.7);
        assert_eq!(hold.carried(), 4_700_000);
        // Still: nothing sent, the estimate sinks to 0.6: still 4.7 carried.
        let (at, bytes) = second(&mut hold, at, bytes, 0, 20, 4, 0.6);
        let (at, _) = second(&mut hold, at, bytes, 0, 20, 4, 0.6);
        assert_eq!(hold.carried(), 4_700_000);
        // A keyframe alone (one frame, 800 kbit at 1 Mbit/s) is not busy either.
        let mut keyframe = Hold::new(8 * MBIT, at);
        second(&mut keyframe, at, 0, 1, 800, 1, 0.6);
        assert_eq!(keyframe.carried(), 8 * MBIT);
    }

    #[test]
    fn motion_after_a_pause_starts_again_from_what_was_carried_once() {
        let start = Instant::now();
        let mut hold = Hold::new(8 * MBIT, start);
        // A drag with the estimate at 6 (not far under the 8 the session started with: no new start yet).
        let (at, _) = second(&mut hold, start, 0, 40, 4500, 5, 6.0);
        assert_eq!((hold.carried(), hold.resets), (6 * MBIT, 0));
        // Two seconds still, the estimate at 0.6: the next change starts again from 6.
        let later = at + Duration::from_secs(2);
        assert_eq!(hold.changed(later, 600_000), Some(6 * MBIT));
        assert_eq!(hold.resets, 1);
        // The next frames of the same motion do not; nor a new pause within RESET_GAP.
        assert_eq!(
            hold.changed(later + Duration::from_millis(20), 600_000),
            None
        );
        assert_eq!(
            hold.changed(later + Duration::from_millis(2100), 600_000),
            None
        );
        // After RESET_GAP and a pause: again.
        assert_eq!(
            hold.changed(later + Duration::from_millis(4000), 600_000),
            Some(6 * MBIT)
        );
        // An estimate near what was carried needs no new start.
        assert_eq!(
            hold.changed(later + Duration::from_secs(9), 5_000_000),
            None
        );
    }

    #[test]
    fn a_link_that_carries_less_lowers_what_is_held() {
        let start = Instant::now();
        let mut hold = Hold::new(8 * MBIT, start);
        // Started again from 8 after a pause, the link takes 2: the busy seconds' estimate falls, and so does what
        // is held, so the next pause starts from 2, not 8.
        let (at, bytes) = second(&mut hold, start, 0, 40, 5000, 6, 3.0);
        let (at, _) = second(&mut hold, at, bytes, 40, 1800, 2, 2.0);
        assert_eq!(hold.carried(), 2 * MBIT);
        assert_eq!(
            hold.changed(at + Duration::from_secs(5), 500_000),
            Some(2 * MBIT)
        );
    }

    #[test]
    fn a_still_screen_after_motion_gets_one_keyframe() {
        let start = Instant::now();
        let mut refresh = Refresh::default();
        // A caret's blink (a few changed frames) earns none.
        for blink in 0..3 {
            refresh.changed(start + Duration::from_millis(blink * 500));
        }
        assert!(!refresh.due(start + Duration::from_secs(3), true));
        // A clock that ticks once a second, for half a minute, earns none either.
        let mut clock = Refresh::default();
        for tick in 0..30 {
            clock.changed(start + Duration::from_secs(tick));
            assert!(!clock.due(
                start + Duration::from_secs(tick) + Duration::from_millis(500),
                true
            ));
        }
        // A drag: 40 frames; not while it goes on, once 300 ms after its last frame, then not again.
        for frame in 0..40 {
            refresh.changed(start + Duration::from_secs(4) + Duration::from_millis(frame * 22));
        }
        let end = start + Duration::from_secs(4) + Duration::from_millis(39 * 22);
        assert!(!refresh.due(end + Duration::from_millis(100), true));
        assert!(refresh.due(end + Duration::from_millis(300), true));
        assert!(!refresh.due(end + Duration::from_millis(900), true));
        assert_eq!(refresh.count, 1);
        // A keyframe sent meanwhile (asked for after a loss) makes the refresh needless.
        for frame in 0..20 {
            refresh.changed(end + Duration::from_secs(2) + Duration::from_millis(frame * 22));
        }
        refresh.keyframe();
        assert!(!refresh.due(end + Duration::from_secs(4), true));
    }
}
