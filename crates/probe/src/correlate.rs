use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use pingwatch_core::ProbeError;
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    pub at: Instant,
    pub kernel: Option<SystemTime>,
}

#[derive(Default)]
pub struct Correlator {
    pending: Mutex<HashMap<u16, oneshot::Sender<Arrival>>>,
    answered: Mutex<VecDeque<u16>>,
    duplicates: AtomicU64,
}

const REMEMBERED_ANSWERS: usize = 1024;

impl Correlator {
    pub fn expect(&self, seq: u16) -> oneshot::Receiver<Arrival> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(seq, tx);
        rx
    }

    pub fn forget(&self, seq: u16) {
        self.pending.lock().unwrap().remove(&seq);
    }

    pub fn arrived(&self, seq: u16, arrival: Arrival) {
        let waiting = self.pending.lock().unwrap().remove(&seq);
        let mut answered = self.answered.lock().unwrap();
        match waiting {
            Some(tx) => {
                let _ = tx.send(arrival);
                answered.push_back(seq);
                while answered.len() > REMEMBERED_ANSWERS {
                    answered.pop_front();
                }
            }
            None if answered.contains(&seq) => {
                self.duplicates.fetch_add(1, Ordering::Relaxed);
            }
            None => {}
        }
    }

    pub fn duplicates(&self) -> u64 {
        self.duplicates.load(Ordering::Relaxed)
    }

    pub async fn wait(
        &self,
        seq: u16,
        rx: oneshot::Receiver<Arrival>,
        sent: Instant,
        sent_wall: SystemTime,
        timeout: Duration,
    ) -> Result<Duration, ProbeError> {
        let left = timeout.saturating_sub(sent.elapsed());
        match tokio::time::timeout(left, rx).await {
            Ok(Ok(arrival)) => Ok(rtt_of(arrival, sent, sent_wall)),
            Ok(Err(_)) => Err(ProbeError::Failed("receiver stopped".into())),
            Err(_) => {
                self.forget(seq);
                Err(ProbeError::Timeout)
            }
        }
    }
}

pub fn rtt_of(arrival: Arrival, sent: Instant, sent_wall: SystemTime) -> Duration {
    let user = arrival.at.saturating_duration_since(sent);
    match arrival
        .kernel
        .and_then(|k| k.duration_since(sent_wall).ok())
    {
        Some(kernel) if kernel <= user + Duration::from_millis(1) => kernel,
        _ => user,
    }
}

pub fn random_u64() -> u64 {
    RandomState::new().hash_one(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_reply_counts_as_a_duplicate() {
        let correlator = Correlator::default();
        let mut rx = correlator.expect(7);
        let arrival = Arrival {
            at: Instant::now(),
            kernel: None,
        };
        correlator.arrived(7, arrival);
        correlator.arrived(7, arrival);
        correlator.arrived(9, arrival);
        assert!(rx.try_recv().is_ok());
        assert_eq!(correlator.duplicates(), 1);
    }

    #[test]
    fn kernel_stamp_is_used_only_when_it_is_plausible() {
        let sent = Instant::now();
        let wall = SystemTime::now();
        let late = Arrival {
            at: sent + Duration::from_millis(10),
            kernel: Some(wall + Duration::from_millis(8)),
        };
        assert_eq!(rtt_of(late, sent, wall), Duration::from_millis(8));
        let jumped = Arrival {
            at: sent + Duration::from_millis(10),
            kernel: Some(wall + Duration::from_secs(5)),
        };
        assert_eq!(rtt_of(jumped, sent, wall), Duration::from_millis(10));
        let backwards = Arrival {
            at: sent + Duration::from_millis(10),
            kernel: Some(wall - Duration::from_secs(5)),
        };
        assert_eq!(rtt_of(backwards, sent, wall), Duration::from_millis(10));
    }
}
