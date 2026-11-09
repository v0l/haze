use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haze_core::config::POLITE_CONNECTIONS_PER_MINUTE;
use haze_core::{Probe, ProbeFuture, Target};
use tokio::time::Instant;

pub struct Limiter {
    spacing: Duration,
    next: Mutex<Option<Instant>>,
}

impl Limiter {
    pub fn new(spacing: Duration) -> Limiter {
        Limiter {
            spacing,
            next: Mutex::new(None),
        }
    }

    pub fn spacing(&self) -> Duration {
        self.spacing
    }

    pub async fn wait(&self) {
        let slot = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            let slot = next.map_or(now, |n| n.max(now));
            *next = Some(slot + self.spacing);
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

pub fn limiters(targets: &[Target]) -> HashMap<String, Arc<Limiter>> {
    let mut rates: HashMap<String, (f64, bool)> = HashMap::new();
    for t in targets.iter().filter(|t| t.probe.opens_connections()) {
        let entry = rates.entry(t.destination()).or_default();
        entry.0 += t.per_minute();
        entry.1 |= t.allow_aggressive;
    }
    rates
        .into_iter()
        .map(|(destination, (rate, aggressive))| {
            let budget = if aggressive {
                rate.max(POLITE_CONNECTIONS_PER_MINUTE)
            } else {
                POLITE_CONNECTIONS_PER_MINUTE
            };
            let spacing = Duration::from_secs_f64(60.0 / budget);
            (destination, Arc::new(Limiter::new(spacing)))
        })
        .collect()
}

pub struct Limited {
    inner: Arc<dyn Probe>,
    limiter: Arc<Limiter>,
}

impl Limited {
    pub fn new(inner: Arc<dyn Probe>, limiter: Arc<Limiter>) -> Limited {
        Limited { inner, limiter }
    }
}

impl Probe for Limited {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            self.limiter.wait().await;
            self.inner.probe(seq, timeout).await
        })
    }

    fn duplicates(&self) -> u64 {
        self.inner.duplicates()
    }
}
