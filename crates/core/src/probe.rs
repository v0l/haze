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
    Refused {
        status: u16,
        retry_after: Option<Duration>,
    },
    Reset(String),
    Failed(String),
}

impl ProbeError {
    pub fn refused(status: u16) -> ProbeError {
        ProbeError::Refused {
            status,
            retry_after: None,
        }
    }

    pub fn backoff_reason(&self) -> Option<String> {
        match self {
            ProbeError::Refused { status, .. } if matches!(status, 429 | 503) => {
                Some(format!("HTTP {status}"))
            }
            ProbeError::Refused { status, .. } => {
                Some(format!("upgrade refused with HTTP {status}"))
            }
            ProbeError::Reset(why) => Some(why.clone()),
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ProbeError::Refused { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Timeout => write!(f, "timed out"),
            ProbeError::Refused { status, .. } => write!(f, "refused with HTTP {status}"),
            ProbeError::Reset(why) => write!(f, "{why}"),
            ProbeError::Failed(why) => write!(f, "{why}"),
        }
    }
}

pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

impl std::error::Error for ProbeError {}

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<Reply, ProbeError>> + Send + 'a>>;

pub trait Probe: Send + Sync {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_>;

    fn duplicates(&self) -> u64 {
        0
    }
}
