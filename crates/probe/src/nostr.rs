use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use haze_core::{Probe, ProbeError, ProbeFuture, Reply};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::correlate::random_u64;
use crate::http::{Url, tls_connector};
use crate::net::{SocketOptions, resolve};
use crate::tcp::{connect, failure};

pub const DEFAULT_EVENT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct Relay {
    url: String,
    parsed: Url,
    event: String,
    family: Option<u8>,
    source: Option<IpAddr>,
    tos: Option<u8>,
    tls: TlsConnector,
}

#[derive(Debug, PartialEq)]
pub enum Answer {
    Event,
    Eose,
    Closed(String),
    Notice(String),
}

pub fn request(subscription: &str, event: &str) -> String {
    format!(r#"["REQ","{subscription}",{{"ids":["{event}"],"limit":1}}]"#)
}

pub fn classify(text: &str, subscription: &str) -> Option<Answer> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let list = value.as_array()?;
    let kind = list.first()?.as_str()?;
    let sub = list.get(1).and_then(|v| v.as_str());
    match kind {
        "EVENT" if sub == Some(subscription) => Some(Answer::Event),
        "EOSE" if sub == Some(subscription) => Some(Answer::Eose),
        "CLOSED" if sub == Some(subscription) => Some(Answer::Closed(
            list.get(2)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )),
        "NOTICE" => Some(Answer::Notice(sub.unwrap_or_default().to_string())),
        _ => None,
    }
}

impl Relay {
    pub fn new(
        url: &str,
        event: Option<&str>,
        family: Option<u8>,
        options: &SocketOptions,
    ) -> Result<Relay, String> {
        let http_form = url
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1);
        if http_form == url {
            return Err(format!("`{url}` is not a ws:// or wss:// url"));
        }
        Ok(Relay {
            url: url.to_string(),
            parsed: http_form.parse()?,
            event: event.unwrap_or(DEFAULT_EVENT).to_string(),
            family,
            source: options.source,
            tos: options.tos,
            tls: tls_connector(),
        })
    }

    async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        stream: S,
        started: Instant,
        mut phases: Vec<(&'static str, Duration)>,
    ) -> Result<Reply, ProbeError> {
        let request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| ProbeError::Failed(e.to_string()))?;
        let before = started.elapsed();
        let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| ProbeError::Failed(format!("WebSocket: {e}")))?;
        let upgraded = started.elapsed();
        phases.push(("upgrade", upgraded - before));
        let subscription = format!("haze-{:016x}", random_u64());
        socket
            .send(Message::text(request_text(&subscription, &self.event)))
            .await
            .map_err(|e| ProbeError::Failed(e.to_string()))?;
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| ProbeError::Failed("relay closed the connection".into()))?
                .map_err(|e| ProbeError::Failed(e.to_string()))?;
            let text = match message {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => {
                    return Err(ProbeError::Failed("relay closed the connection".into()));
                }
                _ => continue,
            };
            let answer = match classify(&text, &subscription) {
                Some(a) => a,
                None => continue,
            };
            let note = match answer {
                Answer::Event => "event",
                Answer::Eose => "eose",
                Answer::Closed(why) => {
                    return Err(ProbeError::Failed(format!(
                        "relay closed the subscription: {why}"
                    )));
                }
                Answer::Notice(_) => continue,
            };
            phases.push(("req", started.elapsed() - upgraded));
            let close = format!(r#"["CLOSE","{subscription}"]"#);
            let _ = socket.send(Message::text(close)).await;
            let _ = socket.close(None).await;
            return Ok(Reply {
                rtt: started.elapsed(),
                phases,
                note: Some(format!("first answer {note}")),
            });
        }
    }

    pub async fn check(&self) -> Result<Reply, ProbeError> {
        let started = Instant::now();
        let ip = resolve(&self.parsed.host, self.family)
            .await
            .map_err(failure)?;
        let dns = started.elapsed();
        let stream = connect(SocketAddr::new(ip, self.parsed.port), self.source, self.tos)
            .await
            .map_err(failure)?;
        let connected = started.elapsed();
        let mut phases = vec![("dns", dns), ("connect", connected - dns)];
        if !self.parsed.tls {
            return self.exchange(stream, started, phases).await;
        }
        let name = ServerName::try_from(self.parsed.host.clone())
            .map_err(|e| ProbeError::Failed(format!("bad server name: {e}")))?;
        let stream = self
            .tls
            .connect(name, stream)
            .await
            .map_err(|e| ProbeError::Failed(format!("TLS: {e}")))?;
        phases.push(("tls", started.elapsed() - connected));
        self.exchange(stream, started, phases).await
    }
}

fn request_text(subscription: &str, event: &str) -> String {
    request(subscription, event)
}

impl Probe for Relay {
    fn probe(&self, _seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            tokio::time::timeout(timeout, self.check())
                .await
                .map_err(|_| ProbeError::Timeout)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_single_id_request() {
        assert_eq!(
            request("haze-1", "ab"),
            r#"["REQ","haze-1",{"ids":["ab"],"limit":1}]"#
        );
    }

    #[test]
    fn classifies_relay_answers() {
        assert_eq!(classify(r#"["EOSE","s"]"#, "s"), Some(Answer::Eose));
        assert_eq!(
            classify(r#"["EVENT","s",{"id":"x"}]"#, "s"),
            Some(Answer::Event)
        );
        assert_eq!(classify(r#"["EOSE","other"]"#, "s"), None);
        assert_eq!(
            classify(r#"["CLOSED","s","auth-required: nope"]"#, "s"),
            Some(Answer::Closed("auth-required: nope".into()))
        );
        assert_eq!(
            classify(r#"["NOTICE","slow down"]"#, "s"),
            Some(Answer::Notice("slow down".into()))
        );
        assert_eq!(classify("not json", "s"), None);
    }

    #[test]
    fn rejects_non_websocket_urls() {
        let none = SocketOptions {
            source: None,
            tos: None,
        };
        assert!(Relay::new("https://relay.example", None, None, &none).is_err());
        let relay = Relay::new("wss://relay.example/path", None, None, &none).unwrap();
        assert_eq!((relay.parsed.port, relay.parsed.tls), (443, true));
    }
}
