use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use haze_core::{Probe, ProbeError};
use haze_probe::SocketOptions;
use haze_probe::dns::Dns;
use haze_probe::nostr::Relay;
use hickory_proto::op::{Message, MessageType, ResponseCode};
use hickory_proto::rr::{RData, Record, rdata::A};
use tokio::net::{TcpListener, UdpSocket};
use tokio_tungstenite::tungstenite::Message as Ws;

const NONE: SocketOptions = SocketOptions {
    source: None,
    tos: None,
};
const EVENT: &str = "d7dd5eb3ab747e16f8d0212d53032ea2a7cadef53837e5a6c66d42849fcb9027";

#[derive(Clone, Copy)]
enum Behaviour {
    Event,
    Eose,
    Closed,
    Silent,
}

async fn fake_relay(behaviour: Behaviour, delay: Duration) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = ws.next().await {
                    let Ws::Text(text) = message else { continue };
                    log.lock().unwrap().push(text.to_string());
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if value[0] != "REQ" {
                        continue;
                    }
                    let sub = value[1].as_str().unwrap().to_string();
                    tokio::time::sleep(delay).await;
                    let _ = ws.send(Ws::text(r#"["NOTICE","welcome"]"#)).await;
                    let _ = ws.send(Ws::text(r#"["EOSE","someone-else"]"#)).await;
                    let reply = match behaviour {
                        Behaviour::Event => format!(r#"["EVENT","{sub}",{{"id":"{EVENT}"}}]"#),
                        Behaviour::Eose => format!(r#"["EOSE","{sub}"]"#),
                        Behaviour::Closed => format!(r#"["CLOSED","{sub}","restricted: no"]"#),
                        Behaviour::Silent => continue,
                    };
                    let _ = ws.send(Ws::text(reply)).await;
                }
            });
        }
    });
    (format!("ws://{address}/"), seen)
}

#[tokio::test]
async fn nostr_times_the_first_event() {
    let delay = Duration::from_millis(30);
    let (url, seen) = fake_relay(Behaviour::Event, delay).await;
    let relay = Relay::new(&url, Some(EVENT), None, &NONE).unwrap();
    let reply = relay.probe(0, Duration::from_secs(2)).await.unwrap();
    assert!(reply.rtt >= delay, "{:?}", reply.rtt);
    assert!(
        reply.rtt < delay + Duration::from_millis(500),
        "{:?}",
        reply.rtt
    );
    assert_eq!(reply.note.as_deref(), Some("first answer event"));
    let phases: Vec<&str> = reply.phases.iter().map(|(n, _)| *n).collect();
    assert_eq!(phases, ["dns", "connect", "upgrade", "req"]);
    let req = reply.phases.iter().find(|(n, _)| *n == "req").unwrap().1;
    assert!(req >= delay);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let seen = seen.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(request[0], "REQ");
    assert!(request[1].as_str().unwrap().starts_with("haze-"));
    assert_eq!(request[2], serde_json::json!({"ids": [EVENT], "limit": 1}));
    assert!(
        seen.iter().any(|m| m.starts_with(r#"["CLOSE""#)),
        "{seen:?}"
    );
}

#[tokio::test]
async fn nostr_accepts_eose_and_reports_closed_and_silence() {
    let (url, _) = fake_relay(Behaviour::Eose, Duration::ZERO).await;
    let reply = Relay::new(&url, None, None, &NONE)
        .unwrap()
        .probe(0, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(reply.note.as_deref(), Some("first answer eose"));

    let (url, _) = fake_relay(Behaviour::Closed, Duration::ZERO).await;
    let error = Relay::new(&url, None, None, &NONE)
        .unwrap()
        .probe(0, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ProbeError::Failed("relay closed the subscription: restricted: no".into())
    );

    let (url, _) = fake_relay(Behaviour::Silent, Duration::ZERO).await;
    let error = Relay::new(&url, None, None, &NONE)
        .unwrap()
        .probe(0, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(error, ProbeError::Timeout);
}

#[tokio::test]
async fn nostr_reports_a_refused_relay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let relay = Relay::new(&format!("ws://{address}"), None, None, &NONE).unwrap();
    assert!(matches!(
        relay.probe(0, Duration::from_secs(1)).await,
        Err(ProbeError::Failed(_))
    ));
}

async fn fake_dns(delay: Duration, code: ResponseCode, decoy: bool) -> SocketAddr {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let address = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let (n, from) = socket.recv_from(&mut buf).await.unwrap();
            let request = Message::from_vec(&buf[..n]).unwrap();
            let socket = socket.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if decoy {
                    let mut wrong = request.clone();
                    wrong.metadata.id = request.metadata.id.wrapping_add(1);
                    wrong.metadata.message_type = MessageType::Response;
                    socket
                        .send_to(&wrong.to_vec().unwrap(), from)
                        .await
                        .unwrap();
                }
                let mut response = request.clone();
                response.metadata.message_type = MessageType::Response;
                response.metadata.response_code = code;
                if code == ResponseCode::NoError {
                    let name = request.queries[0].name().clone();
                    response.answers.push(Record::from_rdata(
                        name,
                        60,
                        RData::A(A::new(192, 0, 2, 7)),
                    ));
                }
                socket
                    .send_to(&response.to_vec().unwrap(), from)
                    .await
                    .unwrap();
            });
        }
    });
    address
}

#[tokio::test]
async fn dns_times_the_matching_answer() {
    let delay = Duration::from_millis(20);
    let server = fake_dns(delay, ResponseCode::NoError, true).await;
    let probe = Dns::new(server, "example.com", "A", NONE).unwrap();
    let reply = probe.probe(0, Duration::from_secs(1)).await.unwrap();
    assert!(reply.rtt >= delay, "{:?}", reply.rtt);
    assert!(
        reply.rtt < delay + Duration::from_millis(300),
        "{:?}",
        reply.rtt
    );
    assert_eq!(reply.note.as_deref(), Some("No Error, 1 answers"));
}

#[tokio::test]
async fn dns_nxdomain_is_an_answer_and_servfail_is_a_failure() {
    let server = fake_dns(Duration::ZERO, ResponseCode::NXDomain, false).await;
    let reply = Dns::new(server, "nope.example", "AAAA", NONE)
        .unwrap()
        .probe(0, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(reply.note.unwrap().starts_with("Non-Existent Domain"));
    let server = fake_dns(Duration::ZERO, ResponseCode::ServFail, false).await;
    let error = Dns::new(server, "example.com", "A", NONE)
        .unwrap()
        .probe(0, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProbeError::Failed(ref why) if why.contains("Server Failure")),
        "{error:?}"
    );
}

#[tokio::test]
async fn dns_silence_is_a_timeout() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let probe = Dns::new(silent.local_addr().unwrap(), "example.com", "A", NONE).unwrap();
    assert_eq!(
        probe.probe(0, Duration::from_millis(200)).await,
        Err(ProbeError::Timeout)
    );
}

async fn refusing_relay(status: u16) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status} Nope\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    format!("ws://{address}/")
}

#[tokio::test]
async fn nostr_refusal_is_not_a_timeout_and_is_counted_apart_from_loss() {
    use haze_probe::Sampler;
    let url = refusing_relay(503).await;
    let relay = Relay::new(&url, None, None, &NONE).unwrap();
    assert_eq!(
        relay.probe(0, Duration::from_secs(2)).await,
        Err(ProbeError::Refused(503))
    );
    let sampler = Sampler::new(Arc::new(relay));
    let (step, detail) = sampler
        .step(3, Duration::from_millis(30), Duration::from_secs(2))
        .await;
    assert_eq!((step.refused, detail.refusals.len()), (3, 3));
    assert_eq!(step.summary().loss, 0.0);

    let (url, _) = fake_relay(Behaviour::Silent, Duration::ZERO).await;
    let silent = Relay::new(&url, None, None, &NONE).unwrap();
    let sampler = Sampler::new(Arc::new(silent));
    let (step, detail) = sampler
        .step(2, Duration::from_millis(30), Duration::from_millis(200))
        .await;
    assert_eq!((step.refused, detail.refusals.len()), (0, 0));
    assert_eq!(step.summary().loss, 1.0);

    let url = refusing_relay(400).await;
    let relay = Relay::new(&url, None, None, &NONE).unwrap();
    assert_eq!(
        relay.probe(0, Duration::from_secs(2)).await,
        Err(ProbeError::Refused(400))
    );
}
