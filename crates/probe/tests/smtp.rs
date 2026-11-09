use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haze_core::{Config, Probe, ProbeError, ProbeKind};
use haze_probe::SocketOptions;
use haze_probe::smtp::Smtp;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const NONE: SocketOptions = SocketOptions {
    source: None,
    tos: None,
};

type Log = Arc<Mutex<Vec<String>>>;

async fn fake_mail_server(banner: &'static str, delay: Duration) -> (SocketAddr, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let log: Log = Arc::default();
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                tokio::time::sleep(delay).await;
                let _ = write.write_all(banner.as_bytes()).await;
                let mut lines = BufReader::new(read).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            seen.lock().unwrap().push(line.clone());
                            if line.eq_ignore_ascii_case("QUIT") {
                                let _ = write.write_all(b"221 2.0.0 Bye\r\n").await;
                            }
                        }
                        Ok(None) => {
                            seen.lock().unwrap().push("fin".into());
                            break;
                        }
                        Err(e) => {
                            seen.lock().unwrap().push(format!("{:?}", e.kind()));
                            break;
                        }
                    }
                }
            });
        }
    });
    (address, log)
}

async fn settled(log: &Log, count: usize) -> Vec<String> {
    for _ in 0..200 {
        if log.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    log.lock().unwrap().clone()
}

#[tokio::test]
async fn smtp_times_the_banner_and_says_quit() {
    let delay = Duration::from_millis(40);
    let (address, log) = fake_mail_server("220 mx.example ESMTP ready\r\n", delay).await;
    let probe = Smtp::new(address, &NONE);
    let reply = probe.probe(0, Duration::from_secs(2)).await.unwrap();
    assert!(reply.rtt >= delay, "{:?}", reply.rtt);
    let names: Vec<&str> = reply.phases.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["connect", "banner"]);
    assert_eq!(reply.note.as_deref(), Some("220 mx.example ESMTP ready"));
    assert_eq!(settled(&log, 2).await, ["QUIT", "fin"]);
}

#[tokio::test]
async fn smtp_reads_a_multiline_banner() {
    let (address, log) = fake_mail_server(
        "220-mx.example first\r\n220 mx.example ready\r\n",
        Duration::ZERO,
    )
    .await;
    let probe = Smtp::new(address, &NONE);
    probe.probe(0, Duration::from_secs(2)).await.unwrap();
    assert_eq!(settled(&log, 2).await, ["QUIT", "fin"]);
}

#[tokio::test]
async fn smtp_421_is_a_refusal_and_other_banners_fail() {
    let (address, log) =
        fake_mail_server("421 4.7.0 too many connections\r\n", Duration::ZERO).await;
    let error = Smtp::new(address, &NONE)
        .probe(0, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(matches!(error, ProbeError::Reset(_)), "{error:?}");
    assert!(error.backoff_reason().unwrap().contains("421"));
    assert_eq!(settled(&log, 2).await, ["QUIT", "fin"]);

    let (address, _) = fake_mail_server("SSH-2.0-OpenSSH\r\n", Duration::ZERO).await;
    let error = Smtp::new(address, &NONE)
        .probe(0, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(matches!(error, ProbeError::Failed(_)), "{error:?}");
}

#[tokio::test]
async fn smtp_silence_is_a_timeout() {
    let (address, _) = fake_mail_server("", Duration::ZERO).await;
    let error = Smtp::new(address, &NONE)
        .probe(0, Duration::from_millis(200))
        .await
        .unwrap_err();
    assert_eq!(error, ProbeError::Timeout);
}

#[test]
fn smtp_targets_default_to_port_25_and_one_ping() {
    let config = Config::parse("[targets.mx]\nhost = \"mx.example\"\nprobe = \"smtp\"").unwrap();
    let mx = &config.targets[0];
    assert_eq!(mx.probe, ProbeKind::Smtp);
    assert_eq!(mx.destination(), "mx.example:25");
    assert_eq!(mx.pings, 1);
    let submission =
        Config::parse("[targets.mx]\nhost = \"mx.example\"\nprobe = \"smtp\"\nport = 587").unwrap();
    assert_eq!(submission.targets[0].destination(), "mx.example:587");
}
