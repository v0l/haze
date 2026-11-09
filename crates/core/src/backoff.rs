use std::time::Duration;

pub const MAX_BACKOFF: Duration = Duration::from_secs(3600);
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);
pub const CLEAN_STEPS_TO_RESET: u32 = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Backoff {
    base: Duration,
    interval: Duration,
    clean: u32,
    reason: Option<String>,
}

impl Backoff {
    pub fn new(base: Duration) -> Backoff {
        Backoff {
            base,
            interval: base,
            clean: 0,
            reason: None,
        }
    }

    pub fn observe(&mut self, pushback: Option<(&str, Option<Duration>)>) -> Duration {
        match pushback {
            Some((reason, retry_after)) => {
                self.interval = (self.interval * 2).min(MAX_BACKOFF.max(self.base));
                self.clean = 0;
                self.reason = Some(reason.to_string());
                let asked = retry_after.unwrap_or_default().min(MAX_RETRY_AFTER);
                self.interval.max(asked)
            }
            None if self.reason.is_some() => {
                self.clean += 1;
                if self.clean >= CLEAN_STEPS_TO_RESET {
                    *self = Backoff::new(self.base);
                }
                self.interval
            }
            None => self.interval,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn pushback_doubles_up_to_an_hour() {
        let mut b = Backoff::new(MINUTE);
        assert_eq!(b.observe(None), MINUTE);
        assert_eq!(b.reason(), None);
        assert_eq!(b.observe(Some(("HTTP 429", None))), MINUTE * 2);
        assert_eq!(b.reason(), Some("HTTP 429"));
        assert_eq!(b.observe(Some(("HTTP 429", None))), MINUTE * 4);
        for _ in 0..10 {
            b.observe(Some(("TCP reset", None)));
        }
        assert_eq!(b.interval(), MAX_BACKOFF);
        assert_eq!(b.reason(), Some("TCP reset"));
    }

    #[test]
    fn retry_after_is_honoured() {
        let mut b = Backoff::new(MINUTE);
        let wait = b.observe(Some(("HTTP 503", Some(Duration::from_secs(900)))));
        assert_eq!(wait, Duration::from_secs(900));
        assert_eq!(b.interval(), MINUTE * 2);
        let short = b.observe(Some(("HTTP 503", Some(Duration::from_secs(5)))));
        assert_eq!(short, MINUTE * 4);
    }

    #[test]
    fn three_clean_steps_reset_it() {
        let mut b = Backoff::new(MINUTE);
        b.observe(Some(("HTTP 429", None)));
        b.observe(Some(("HTTP 429", None)));
        assert_eq!(b.observe(None), MINUTE * 4);
        assert_eq!(b.observe(None), MINUTE * 4);
        assert_eq!(b.reason(), Some("HTTP 429"));
        assert_eq!(b.observe(None), MINUTE);
        assert_eq!(b.reason(), None);
        b.observe(Some(("HTTP 429", None)));
        b.observe(None);
        b.observe(None);
        b.observe(Some(("HTTP 429", None)));
        assert_eq!(
            b.interval(),
            MINUTE * 4,
            "a refusal restarts the clean count"
        );
    }
}
