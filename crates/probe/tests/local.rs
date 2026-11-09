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

#[cfg(any(
    all(target_os = "linux", target_pointer_width = "64"),
    target_vendor = "apple"
))]
#[test]
fn udp_receive_carries_a_kernel_stamp() {
    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    haze_probe::net::enable_timestamps(&socket2::SockRef::from(&receiver));
    let before = std::time::SystemTime::now();
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.send_to(b"stamp", receiver.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 64];
    let (n, stamp) =
        haze_probe::net::recv_stamped(&socket2::SockRef::from(&receiver), &mut buf).unwrap();
    let after = std::time::SystemTime::now();
    assert_eq!(&buf[..n], b"stamp");
    let stamp = stamp.expect("no kernel receive stamp");
    assert!(
        stamp + Duration::from_millis(1) >= before && stamp <= after,
        "{before:?} {stamp:?} {after:?}"
    );
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
async fn tcp_connect_refused_is_a_reset() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let probe = TcpConnect::new(address, &NO_OPTIONS);
    let error = probe.probe(0, Duration::from_secs(1)).await.unwrap_err();
    assert!(matches!(error, ProbeError::Reset(_)), "{error:?}");
    assert!(error.backoff_reason().is_some());
}

type Seen = Arc<std::sync::Mutex<Vec<String>>>;

async fn recording_server(answer: fn(&str) -> &'static str) -> (SocketAddr, Seen, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests: Seen = Arc::default();
    let closes: Seen = Arc::default();
    let (r, c) = (requests.clone(), closes.clone());
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (r, c) = (r.clone(), c.clone());
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = answer(&request);
                r.lock().unwrap().push(request);
                let _ = stream.write_all(reply.as_bytes()).await;
                let close = loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => break "fin".to_string(),
                        Ok(_) => continue,
                        Err(e) => break format!("{:?}", e.kind()),
                    }
                };
                c.lock().unwrap().push(close);
            });
        }
    });
    (address, requests, closes)
}

async fn settle(seen: &Seen, count: usize) -> Vec<String> {
    for _ in 0..200 {
        if seen.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn http_sends_head_with_a_polite_user_agent_and_closes_with_fin() {
    let (address, requests, closes) =
        recording_server(|_| "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n").await;
    let probe = Http::new(&format!("http://{address}/health"), None, &NO_OPTIONS).unwrap();
    let reply = probe.probe(0, Duration::from_secs(2)).await.unwrap();
    assert_eq!(reply.note.as_deref(), Some("HTTP 200"));
    let requests = settle(&requests, 1).await;
    assert!(
        requests[0].starts_with("HEAD /health HTTP/1.1\r\n"),
        "{requests:?}"
    );
    let agent = format!(
        "User-Agent: haze/{} (+https://github.com/v0l/haze)\r\n",
        env!("CARGO_PKG_VERSION")
    );
    assert!(requests[0].contains(&agent), "{requests:?}");
    assert_eq!(settle(&closes, 1).await, ["fin"]);
}

#[tokio::test]
async fn http_falls_back_to_get_when_head_is_not_allowed() {
    let (address, requests, closes) = recording_server(|request| {
        if request.starts_with("HEAD") {
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n"
        } else {
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
        }
    })
    .await;
    let probe = Http::new(&format!("http://{address}/"), None, &NO_OPTIONS).unwrap();
    probe.probe(0, Duration::from_secs(2)).await.unwrap();
    probe.probe(1, Duration::from_secs(2)).await.unwrap();
    let methods: Vec<String> = settle(&requests, 3)
        .await
        .iter()
        .map(|r| r.split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(methods, ["HEAD", "GET", "GET"]);
    assert_eq!(settle(&closes, 3).await, ["fin", "fin", "fin"]);
}

#[tokio::test]
async fn http_uses_the_configured_method() {
    let (address, requests, _) =
        recording_server(|_| "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let probe = Http::new(&format!("http://{address}/"), None, &NO_OPTIONS)
        .unwrap()
        .with_method(Some("GET"))
        .unwrap();
    probe.probe(0, Duration::from_secs(2)).await.unwrap();
    assert!(settle(&requests, 1).await[0].starts_with("GET / "));
    assert!(
        Http::new("http://x/", None, &NO_OPTIONS)
            .unwrap()
            .with_method(Some("DELETE"))
            .is_err()
    );
}

#[tokio::test]
async fn tcp_connect_closes_with_fin_even_after_a_banner() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let closes: Seen = Arc::default();
    let seen = closes.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                if let Err(e) = stream.write_all(b"more\r\n").await {
                    seen.lock().unwrap().push(format!("{:?}", e.kind()));
                    return;
                }
                let mut buf = [0u8; 256];
                let close = loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => break "fin".to_string(),
                        Ok(_) => continue,
                        Err(e) => break format!("{:?}", e.kind()),
                    }
                };
                seen.lock().unwrap().push(close);
            });
        }
    });
    let probe = TcpConnect::new(address, &NO_OPTIONS);
    for seq in 0..3 {
        probe.probe(seq, Duration::from_secs(1)).await.unwrap();
    }
    assert_eq!(settle(&closes, 3).await, ["fin"; 3]);
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
    let probe = Http::new(
        &format!("http://{}/", answering("500 Oops", "").await),
        None,
        &NO_OPTIONS,
    )
    .unwrap();
    let error = probe.probe(0, Duration::from_secs(1)).await.unwrap_err();
    assert_eq!(error, ProbeError::Failed("HTTP 500".into()));
    assert_eq!(error.backoff_reason(), None);
}

async fn answering(status: &'static str, headers: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let reply = format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\n\r\n");
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    address
}

#[tokio::test]
async fn http_429_and_503_are_refusals_with_retry_after() {
    let busy = answering("429 Too Many Requests", "Retry-After: 120\r\n").await;
    let probe = Http::new(&format!("http://{busy}/"), None, &NO_OPTIONS).unwrap();
    let error = probe.probe(0, Duration::from_secs(1)).await.unwrap_err();
    assert_eq!(
        error,
        ProbeError::Refused {
            status: 429,
            retry_after: Some(Duration::from_secs(120))
        }
    );
    assert_eq!(error.backoff_reason().as_deref(), Some("HTTP 429"));
    let down = answering("503 Busy", "").await;
    let probe = Http::new(&format!("http://{down}/"), None, &NO_OPTIONS).unwrap();
    assert_eq!(
        probe.probe(0, Duration::from_secs(1)).await,
        Err(ProbeError::Refused {
            status: 503,
            retry_after: None
        })
    );
}

#[tokio::test]
async fn a_tls_alert_is_a_reset() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let _ = stream
            .write_all(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28])
            .await;
    });
    let probe = Http::new(&format!("https://{address}/"), None, &NO_OPTIONS).unwrap();
    let error = probe.probe(0, Duration::from_secs(1)).await.unwrap_err();
    assert!(matches!(error, ProbeError::Reset(_)), "{error:?}");
    assert!(
        error.backoff_reason().unwrap().contains("TLS alert"),
        "{error:?}"
    );
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
