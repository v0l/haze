use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use haze_core::{Probe, ProbeError};
use haze_probe::SocketOptions;
use haze_probe::dns::Dns;
use haze_probe::http::Http;
use hickory_proto::op::{Message, MessageType, ResponseCode};
use hickory_proto::rr::{RData, Record, rdata::A};
use tokio::net::{TcpListener, UdpSocket};

const NONE: SocketOptions = SocketOptions {
    source: None,
    tos: None,
};

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

async fn refusing_server(status: u16) -> String {
    refusing_server_with(status, "").await
}

async fn refusing_server_with(status: u16, headers: &'static str) -> String {
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
                    "HTTP/1.1 {status} Nope\r\n{headers}content-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    format!("http://{address}/")
}

async fn silent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            held.push(stream);
        }
    });
    format!("http://{address}/")
}

#[tokio::test]
async fn http_refusal_is_not_a_timeout_and_is_counted_apart_from_loss() {
    use haze_probe::Sampler;
    let url = refusing_server(503).await;
    let http = Http::new(&url, None, &NONE).unwrap();
    assert_eq!(
        http.probe(0, Duration::from_secs(2)).await,
        Err(ProbeError::Refused {
            status: 503,
            retry_after: None
        })
    );
    let sampler = Sampler::new(Arc::new(http));
    let (step, detail) = sampler
        .step(3, Duration::from_millis(30), Duration::from_secs(2))
        .await;
    assert_eq!((step.refused, detail.refusals.len()), (3, 3));
    assert_eq!(step.summary().loss, 0.0);

    let url = silent_server().await;
    let silent = Http::new(&url, None, &NONE).unwrap();
    let sampler = Sampler::new(Arc::new(silent));
    let (step, detail) = sampler
        .step(2, Duration::from_millis(30), Duration::from_millis(200))
        .await;
    assert_eq!((step.refused, detail.refusals.len()), (0, 0));
    assert_eq!(step.summary().loss, 1.0);
}

#[tokio::test]
async fn a_refusal_carries_retry_after() {
    let url = refusing_server_with(429, "retry-after: 90\r\n").await;
    let http = Http::new(&url, None, &NONE).unwrap();
    let error = http.probe(0, Duration::from_secs(2)).await.unwrap_err();
    assert_eq!(
        error,
        ProbeError::Refused {
            status: 429,
            retry_after: Some(Duration::from_secs(90))
        }
    );
    assert_eq!(error.backoff_reason().as_deref(), Some("HTTP 429"));
}
