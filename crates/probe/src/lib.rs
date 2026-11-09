pub mod correlate;
pub mod dns;
pub mod http;
#[cfg(unix)]
pub mod icmp;
pub mod net;
pub mod nostr;
pub mod tcp;
pub mod udp;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use haze_core::sample::count_reordered;
use haze_core::{Probe, ProbeError, ProbeKind, Reply, Step, Target};
use tokio::task::JoinSet;

pub use net::SocketOptions;

pub async fn build(target: &Target) -> Result<Arc<dyn Probe>, String> {
    let source = target
        .source
        .as_deref()
        .map(|s| {
            s.parse::<IpAddr>()
                .map_err(|_| format!("`{s}` is not a source address"))
        })
        .transpose()?;
    let options = SocketOptions {
        source,
        tos: target.tos,
    };
    let address = || async {
        let host = target.host.as_deref().unwrap_or_default();
        net::resolve(host, target.family)
            .await
            .map_err(|e| format!("{}: {e}", target.path))
    };
    Ok(match target.probe {
        #[cfg(unix)]
        ProbeKind::Icmp => Arc::new(
            icmp::Icmp::new(address().await?, target.size, &options).map_err(|e| e.to_string())?,
        ),
        #[cfg(not(unix))]
        ProbeKind::Icmp => return Err("ICMP is not supported on this platform yet".into()),
        ProbeKind::Tcp => Arc::new(tcp::TcpConnect::new(
            SocketAddr::new(address().await?, target.port.unwrap_or_default()),
            &options,
        )),
        ProbeKind::Udp => Arc::new(
            udp::UdpEcho::new(
                SocketAddr::new(address().await?, target.port.unwrap_or_default()),
                target.size,
                &options,
            )
            .await
            .map_err(|e| e.to_string())?,
        ),
        ProbeKind::Http => Arc::new(http::Http::new(
            target.url.as_deref().unwrap_or_default(),
            target.family,
            &options,
        )?),
        ProbeKind::Nostr => Arc::new(nostr::Relay::new(
            target.url.as_deref().unwrap_or_default(),
            target.event.as_deref(),
            target.family,
            &options,
        )?),
        ProbeKind::Dns => Arc::new(dns::Dns::new(
            SocketAddr::new(address().await?, target.port.unwrap_or(53)),
            target.name.as_deref().unwrap_or_default(),
            target.record.as_deref().unwrap_or("A"),
            options,
        )?),
    })
}

pub struct Sampler {
    probe: Arc<dyn Probe>,
    next_seq: AtomicU16,
}

#[derive(Debug, Default)]
pub struct StepDetail {
    pub errors: Vec<(u16, String)>,
    pub refusals: Vec<(u16, u16)>,
    pub last_reply: Option<Reply>,
}

impl Sampler {
    pub fn new(probe: Arc<dyn Probe>) -> Sampler {
        Sampler {
            probe,
            next_seq: AtomicU16::new(0),
        }
    }

    pub async fn step(
        &self,
        pings: u32,
        interval: Duration,
        timeout: Duration,
    ) -> (Step, StepDetail) {
        let start_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        let started = Instant::now();
        let duplicates_before = self.probe.duplicates();
        let gap = interval / pings.max(1);
        let mut tasks = JoinSet::new();
        for index in 0..pings {
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            let probe = self.probe.clone();
            let at = started + gap * index;
            tasks.spawn(async move {
                tokio::time::sleep_until(at.into()).await;
                let sent = started.elapsed();
                (index, seq, sent, probe.probe(seq, timeout).await)
            });
        }
        let mut rtts = vec![None; pings as usize];
        let mut arrivals = Vec::new();
        let mut detail = StepDetail::default();
        while let Some(joined) = tasks.join_next().await {
            let Ok((index, seq, sent, result)) = joined else {
                continue;
            };
            match result {
                Ok(reply) => {
                    rtts[index as usize] = Some(reply.rtt);
                    arrivals.push((index as u16, sent + reply.rtt));
                    detail.last_reply = Some(reply);
                }
                Err(e) => {
                    if let ProbeError::Refused(status) = e {
                        detail.refusals.push((seq, status));
                    }
                    detail.errors.push((seq, e.to_string()));
                }
            }
        }
        let step = Step {
            start_ms,
            step_ms: interval.as_millis() as u32,
            rtts,
            duplicates: (self.probe.duplicates() - duplicates_before) as u32,
            reordered: count_reordered(&arrivals),
            refused: detail.refusals.len() as u32,
        };
        (step, detail)
    }
}
