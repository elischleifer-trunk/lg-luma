//! Return to Home when the HDMI input in front loses its source, e.g. a console switched off.
//!
//! Only a source that was seen with signal and then lost it counts: an input the user picked
//! while its source is still asleep (or booting) is left alone. The loss must also last a few
//! seconds, so mode switches and HDCP renegotiation, which drop the signal briefly, do not
//! close the input.
use crate::model::Input;
use std::time::{Duration, Instant};

/// What the TV reported in one poll: the foreground app and the HDMI inputs.
pub type Snapshot = (Option<String>, Vec<Input>);

#[derive(Default)]
pub struct Watch {
    next: Option<Instant>,
    in_flight: bool,
    /// The HDMI app in front that has shown a signal since it came forward.
    armed: Option<String>,
    /// When the armed input was first seen without its source.
    lost_since: Option<Instant>,
}

impl Watch {
    const INTERVAL: Duration = Duration::from_secs(2);
    const RETRY: Duration = Duration::from_secs(30);
    pub const GRACE: Duration = Duration::from_secs(5);

    /// Whether to poll now. `active`: another app (not Luma) is in front.
    pub fn due(&mut self, now: Instant, active: bool) -> bool {
        if !active {
            self.next = None;
            self.armed = None;
            self.lost_since = None;
            return false;
        }
        if self.in_flight || now < *self.next.get_or_insert(now + Self::INTERVAL) {
            return false;
        }
        self.in_flight = true;
        true
    }

    /// Takes one poll's result; returns whether to go Home.
    pub fn completed(
        &mut self,
        now: Instant,
        result: Result<Snapshot, String>,
    ) -> Result<bool, String> {
        self.in_flight = false;
        let (foreground, inputs) = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.next = Some(now + Self::RETRY);
                return Err(error);
            }
        };
        self.next = Some(now + Self::INTERVAL);
        let input = foreground
            .as_deref()
            .and_then(|id| inputs.iter().find(|input| input.id == id));
        let Some(input) = input else {
            self.armed = None;
            self.lost_since = None;
            return Ok(false);
        };
        let lost = input.signal == Some(false) || input.connected == Some(false);
        if !lost {
            self.armed = Some(input.id.clone());
            self.lost_since = None;
            return Ok(false);
        }
        if self.armed.as_deref() != Some(input.id.as_str()) {
            // Never seen with a signal (or another input came forward): leave it alone.
            self.armed = None;
            self.lost_since = None;
            return Ok(false);
        }
        let since = *self.lost_since.get_or_insert(now);
        if now.duration_since(since) < Self::GRACE {
            return Ok(false);
        }
        self.armed = None;
        self.lost_since = None;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str, connected: Option<bool>, signal: Option<bool>) -> Input {
        Input {
            id: id.into(),
            title: "HDMI 1".into(),
            port: "HDMI 1".into(),
            connected,
            signal,
            device: None,
        }
    }

    fn poll(watch: &mut Watch, now: Instant, foreground: &str, inputs: Vec<Input>) -> bool {
        watch.in_flight = true;
        watch
            .completed(now, Ok((Some(foreground.into()), inputs)))
            .unwrap()
    }

    const HDMI: &str = "com.webos.app.hdmi1";

    #[test]
    fn a_source_that_switches_off_returns_home_after_the_grace_period() {
        let mut watch = Watch::default();
        let start = Instant::now();
        assert!(!poll(
            &mut watch,
            start,
            HDMI,
            vec![input(HDMI, Some(true), Some(true))]
        ));
        let off = start + Duration::from_secs(2);
        assert!(!poll(
            &mut watch,
            off,
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));
        assert!(!poll(
            &mut watch,
            off + Duration::from_secs(2),
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));
        assert!(poll(
            &mut watch,
            off + Watch::GRACE,
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));
        // Once only.
        assert!(!poll(
            &mut watch,
            off + Watch::GRACE * 2,
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));
    }

    #[test]
    fn a_brief_drop_does_not_leave_the_input() {
        let mut watch = Watch::default();
        let start = Instant::now();
        let on = || vec![input(HDMI, Some(true), Some(true))];
        let off = || vec![input(HDMI, Some(true), Some(false))];
        assert!(!poll(&mut watch, start, HDMI, on()));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(2),
            HDMI,
            off()
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(4),
            HDMI,
            on()
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(6),
            HDMI,
            off()
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(10),
            HDMI,
            off()
        ));
        assert!(poll(
            &mut watch,
            start + Duration::from_secs(11),
            HDMI,
            off()
        ));
    }

    #[test]
    fn an_input_opened_without_a_signal_is_left_alone() {
        let mut watch = Watch::default();
        let start = Instant::now();
        for seconds in [0, 2, 10, 60] {
            assert!(!poll(
                &mut watch,
                start + Duration::from_secs(seconds),
                HDMI,
                vec![input(HDMI, Some(true), Some(false))]
            ));
        }
    }

    #[test]
    fn an_unplugged_cable_counts_without_signal_reports() {
        let mut watch = Watch::default();
        let start = Instant::now();
        assert!(!poll(
            &mut watch,
            start,
            HDMI,
            vec![input(HDMI, Some(true), None)]
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(1),
            HDMI,
            vec![input(HDMI, Some(false), None)]
        ));
        assert!(poll(
            &mut watch,
            start + Duration::from_secs(7),
            HDMI,
            vec![input(HDMI, Some(false), None)]
        ));
    }

    #[test]
    fn other_apps_and_luma_in_front_reset_the_watch() {
        let mut watch = Watch::default();
        let start = Instant::now();
        assert!(!poll(
            &mut watch,
            start,
            HDMI,
            vec![input(HDMI, Some(true), Some(true))]
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(1),
            "netflix",
            vec![input(HDMI, Some(true), Some(false))]
        ));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(9),
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));

        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(10),
            HDMI,
            vec![input(HDMI, Some(true), Some(true))]
        ));
        assert!(!watch.due(start + Duration::from_secs(11), false));
        assert!(!poll(
            &mut watch,
            start + Duration::from_secs(20),
            HDMI,
            vec![input(HDMI, Some(true), Some(false))]
        ));
    }

    #[test]
    fn failures_back_off_and_polls_do_not_overlap() {
        let mut watch = Watch::default();
        let start = Instant::now();
        assert!(!watch.due(start, true));
        assert!(watch.due(start + Watch::INTERVAL, true));
        assert!(!watch.due(start + Watch::INTERVAL * 2, true));
        assert!(
            watch
                .completed(start + Watch::INTERVAL, Err("denied".into()))
                .is_err()
        );
        assert!(!watch.due(start + Watch::INTERVAL * 3, true));
        assert!(watch.due(start + Watch::INTERVAL + Watch::RETRY, true));
    }
}
