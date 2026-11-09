use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use haze_core::{Probe, ProbeError};
use haze_probe::http::Http;
use haze_probe::tcp::TcpConnect;
use haze_probe::udp::UdpEcho;
use haze_probe::{Sampler, SocketOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const SCHEDULER_STALL_ALLOWANCE: Duration = Duration::from_secs(2);

const NO_OPTIONS: SocketOptions = SocketOptions {
    source: None,
    tos: None,
};

async fn udp_echo(bind: IpAddr, delay: Duration, copies: usize) -> SocketAddr {
    let socket = Arc::new(UdpSocket::bind(SocketAddr::new(bind, 0)).await.unwrap());
    let address = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let (n, from) = socket.recv_from(&mut buf).await.unwrap();
            let packet = buf[..n].to_vec();
            let socket = socket.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                for _ in 0..copies {
                    socket.send_to(&packet, from).await.unwrap();
                }
            });
        }
    });
    address
}

type Holds = Arc<std::sync::Mutex<Vec<Duration>>>;

fn exact_delay_udp_echo(delay: Duration) -> (SocketAddr, Holds) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let address = socket.local_addr().unwrap();
    let holds: Holds = Arc::default();
    let record = holds.clone();
    haze_probe::net::enable_timestamps(&socket2::SockRef::from(&socket));
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((_, from)) = socket.peek_from(&mut buf) else {
                std::hint::spin_loop();
                continue;
            };
            let Ok((n, stamp)) =
                haze_probe::net::recv_stamped(&socket2::SockRef::from(&socket), &mut buf)
            else {
                continue;
            };
            let arrived = stamp.unwrap_or_else(std::time::SystemTime::now);
            let due = arrived + delay;
            while std::time::SystemTime::now() < due {
                std::hint::spin_loop();
            }
            let held = std::time::SystemTime::now().duration_since(arrived);
            record.lock().unwrap().push(held.unwrap_or_default());
            let _ = socket.send_to(&buf[..n], from);
        }
    });
    (address, holds)
}

#[tokio::test]
async fn udp_echo_measures_an_added_delay() {
    let delay = Duration::from_millis(25);
    let (server, holds) = exact_delay_udp_echo(delay);
    let probe = Arc::new(UdpEcho::new(server, 64, &NO_OPTIONS).await.unwrap());
    let sampler = Sampler::new(probe);
    let (step, detail) = sampler
        .step(10, Duration::from_secs(1), Duration::from_secs(1))
        .await;
    assert!(detail.errors.is_empty(), "{:?}", detail.errors);
    let summary = step.summary();
    assert_eq!(summary.received, 10);
    let min = summary.min.unwrap();
    let median = summary.median.unwrap();
    let max = summary.max.unwrap();
    let holds = holds.lock().unwrap().clone();
    let rtts: Vec<Duration> = step.rtts.iter().flatten().copied().collect();
    assert_eq!(holds.len(), rtts.len(), "holds {holds:?} rtts {rtts:?}");
    let numbers =
        format!("min {min:?} median {median:?} max {max:?} holds {holds:?} rtts {rtts:?}");
    assert!(min >= delay, "min below the server delay: {numbers}");
    let mut overheads: Vec<Duration> = rtts
        .iter()
        .zip(&holds)
        .map(|(rtt, hold)| rtt.saturating_sub(*hold))
        .collect();
    overheads.sort();
    assert!(
        overheads[0] < Duration::from_millis(1),
        "best overhead over the server hold: {numbers}"
    );
    assert!(
        overheads[overheads.len() / 2] < Duration::from_millis(3),
        "median overhead over the server hold: {numbers}"
    );
    assert_eq!(step.duplicates, 0);
}

#[tokio::test]
async fn udp_echo_counts_duplicates_and_works_over_ipv6() {
    let server = udp_echo(Ipv6Addr::LOCALHOST.into(), Duration::ZERO, 2).await;
    let probe = Arc::new(UdpEcho::new(server, 32, &NO_OPTIONS).await.unwrap());
    let sampler = Sampler::new(probe);
    let (step, _) = sampler
        .step(5, Duration::from_millis(500), Duration::from_millis(400))
        .await;
    assert_eq!(step.received(), 5);
    assert_eq!(step.duplicates, 5);
}

#[tokio::test]
async fn udp_echo_reports_loss_as_a_timeout() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let probe = UdpEcho::new(silent.local_addr().unwrap(), 32, &NO_OPTIONS)
        .await
        .unwrap();
    let result = probe.probe(1, Duration::from_millis(100)).await;
    assert_eq!(result, Err(ProbeError::Timeout));
}

#[tokio::test]
async fn tcp_connect_to_a_local_listener() {
    for bind in ["127.0.0.1:0", "[::1]:0"] {
        let listener = TcpListener::bind(bind).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await;
            }
        });
        let probe = TcpConnect::new(address, &NO_OPTIONS);
        let reply = probe.probe(0, Duration::from_secs(1)).await.unwrap();
        assert!(
            reply.rtt < SCHEDULER_STALL_ALLOWANCE,
            "{bind}: {:?}",
            reply.rtt
        );
    }
}

#[tokio::test]
async fn tcp_connect_refused_is_a_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let probe = TcpConnect::new(address, &NO_OPTIONS);
    assert!(matches!(
        probe.probe(0, Duration::from_secs(1)).await,
        Err(ProbeError::Failed(_))
    ));
}

#[tokio::test]
async fn http_timing_against_a_slow_local_server() {
    let delay = Duration::from_millis(30);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                tokio::time::sleep(delay).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                    )
                    .await;
            });
        }
    });
    let probe = Http::new(&format!("http://{address}/health"), None, &NO_OPTIONS).unwrap();
    let timing = probe.fetch().await.unwrap();
    assert_eq!(timing.status, 200);
    assert!(timing.ttfb >= delay, "ttfb {:?}", timing.ttfb);
    assert!(
        timing.total < delay + SCHEDULER_STALL_ALLOWANCE,
        "total {:?}",
        timing.total
    );
    assert!(timing.connect < SCHEDULER_STALL_ALLOWANCE);
    assert_eq!(timing.tls, None);
    let reply = probe.probe(0, Duration::from_secs(1)).await.unwrap();
    let names: Vec<&str> = reply.phases.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["dns", "connect", "ttfb"]);
}

#[tokio::test]
async fn http_error_status_is_a_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        let _ = stream
            .write_all(b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\n\r\n")
            .await;
    });
    let probe = Http::new(&format!("http://{address}/"), None, &NO_OPTIONS).unwrap();
    let error = probe.probe(0, Duration::from_secs(1)).await.unwrap_err();
    assert_eq!(error, ProbeError::Failed("HTTP 503".into()));
}

#[tokio::test]
async fn pings_are_spread_across_the_interval() {
    let server = udp_echo(Ipv4Addr::LOCALHOST.into(), Duration::ZERO, 1).await;
    let probe = Arc::new(UdpEcho::new(server, 32, &NO_OPTIONS).await.unwrap());
    let sampler = Sampler::new(probe);
    let started = std::time::Instant::now();
    let (step, _) = sampler
        .step(4, Duration::from_millis(800), Duration::from_millis(200))
        .await;
    let took = started.elapsed();
    assert_eq!(step.received(), 4);
    assert!(
        took >= Duration::from_millis(600),
        "a burst took only {took:?}"
    );
    assert!(
        took < Duration::from_millis(800) + SCHEDULER_STALL_ALLOWANCE,
        "took {took:?}"
    );
}

#[cfg(unix)]
mod icmp {
    use super::*;
    use haze_probe::icmp::{Icmp, unprivileged_ping_allowed};

    fn allowed() -> bool {
        match unprivileged_ping_allowed() {
            Ok(()) => true,
            Err(why) => {
                eprintln!("skipping the ICMP test: {why}");
                false
            }
        }
    }

    #[tokio::test]
    async fn ping_localhost_v4() {
        if !allowed() {
            return;
        }
        let probe = Arc::new(Icmp::new(Ipv4Addr::LOCALHOST.into(), 56, &NO_OPTIONS).unwrap());
        let (step, detail) = Sampler::new(probe)
            .step(5, Duration::from_millis(500), Duration::from_millis(400))
            .await;
        assert!(detail.errors.is_empty(), "{:?}", detail.errors);
        assert_eq!(step.received(), 5);
        assert!(step.summary().median.unwrap() < Duration::from_millis(5));
    }

    #[tokio::test]
    async fn ping_localhost_v6() {
        if !allowed() {
            return;
        }
        let probe = match Icmp::new(Ipv6Addr::LOCALHOST.into(), 56, &NO_OPTIONS) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("skipping the ICMPv6 test: {e}");
                return;
            }
        };
        let (step, detail) = Sampler::new(probe)
            .step(5, Duration::from_millis(500), Duration::from_millis(400))
            .await;
        assert!(detail.errors.is_empty(), "{:?}", detail.errors);
        assert_eq!(step.received(), 5);
    }

    #[tokio::test]
    async fn two_pingers_to_the_same_host_do_not_steal_replies() {
        if !allowed() {
            return;
        }
        let a = Arc::new(Icmp::new(Ipv4Addr::LOCALHOST.into(), 56, &NO_OPTIONS).unwrap());
        let b = Arc::new(Icmp::new(Ipv4Addr::LOCALHOST.into(), 56, &NO_OPTIONS).unwrap());
        let (sa, sb) = (Sampler::new(a.clone()), Sampler::new(b.clone()));
        let interval = Duration::from_millis(500);
        let timeout = Duration::from_millis(400);
        let ((step_a, _), (step_b, _)) =
            tokio::join!(sa.step(5, interval, timeout), sb.step(5, interval, timeout));
        assert_eq!(step_a.received(), 5);
        assert_eq!(step_b.received(), 5);
        assert_eq!(step_a.duplicates + step_b.duplicates, 0);
    }
}
