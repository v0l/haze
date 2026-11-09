use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub rtt: Duration,
    pub phases: Vec<(&'static str, Duration)>,
    pub note: Option<String>,
}

impl Reply {
    pub fn rtt(rtt: Duration) -> Self {
        Self {
            rtt,
            phases: Vec::new(),
            note: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProbeError {
    Timeout,
    Refused(u16),
    Failed(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Timeout => write!(f, "timed out"),
            ProbeError::Refused(status) => write!(f, "refused with HTTP {status}"),
            ProbeError::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for ProbeError {}

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<Reply, ProbeError>> + Send + 'a>>;

pub trait Probe: Send + Sync {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_>;

    fn duplicates(&self) -> u64 {
        0
    }
}
