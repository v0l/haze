use std::time::Duration;

use haze_core::config::NotifyRoute;
use haze_probe::http::Http;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const ATTEMPTS: u32 = 3;
const TIMEOUT: Duration = Duration::from_secs(10);

pub type Route = NotifyRoute;

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub target: String,
    pub alert: String,
    pub event: String,
    pub at_ms: i64,
    pub comment: Option<String>,
    pub loss: f64,
    pub median_ms: Option<f64>,
}

pub struct Notifier {
    tx: Option<mpsc::UnboundedSender<(Message, Vec<String>)>>,
    worker: Option<JoinHandle<()>>,
}

pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn headline(m: &Message) -> String {
    let what = match m.event.as_str() {
        "raised" => "raised",
        "cleared" => "cleared",
        "flapping" => "is flapping",
        "settled-active" => "settled, still raised",
        _ => "settled, clear",
    };
    format!("{} {} {what}", m.target, m.alert)
}

fn detail(m: &Message) -> String {
    let median = m
        .median_ms
        .map(|v| format!("{v:.1} ms median"))
        .unwrap_or_else(|| "no replies".into());
    let mut text = format!("{median}, {:.0}% loss in the last step", m.loss * 100.0);
    if let Some(comment) = &m.comment {
        text = format!("{comment}. {text}");
    }
    text
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn request(route: &Route, m: &Message) -> Request {
    let mut headers = Vec::new();
    if let Some(token) = &route.token {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    match route.kind.as_str() {
        "ntfy" => {
            let urgent = matches!(m.event.as_str(), "raised" | "flapping");
            headers.push(("Title".into(), headline(m).replace(['\r', '\n'], " ")));
            headers.push((
                "Priority".into(),
                if urgent { "high" } else { "default" }.into(),
            ));
            headers.push((
                "Tags".into(),
                if urgent {
                    "rotating_light"
                } else {
                    "white_check_mark"
                }
                .into(),
            ));
            headers.push(("Content-Type".into(), "text/plain; charset=utf-8".into()));
            Request {
                url: route.url.clone(),
                headers,
                body: detail(m).into_bytes(),
            }
        }
        _ => {
            headers.push(("Content-Type".into(), "application/json".into()));
            let median = m
                .median_ms
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".into());
            let comment = m
                .comment
                .as_deref()
                .map(json_string)
                .unwrap_or_else(|| "null".into());
            let body = format!(
                "{{\"source\":\"haze\",\"target\":{},\"alert\":{},\"event\":{},\"at_ms\":{},\"text\":{},\"comment\":{comment},\"loss\":{},\"median_ms\":{median}}}",
                json_string(&m.target),
                json_string(&m.alert),
                json_string(&m.event),
                m.at_ms,
                json_string(&format!("{}: {}", headline(m), detail(m))),
                m.loss,
            );
            Request {
                url: route.url.clone(),
                headers,
                body: body.into_bytes(),
            }
        }
    }
}

pub fn wants(route: &Route, tags: &[String]) -> bool {
    route.tags.is_empty() || route.tags.iter().any(|t| tags.contains(t))
}

pub async fn deliver(route: &Route, m: &Message) -> Result<u16, String> {
    let req = request(route, m);
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
        }
        let http = Http::post(&req.url, req.headers.clone(), req.body.clone())?;
        match tokio::time::timeout(TIMEOUT, http.fetch()).await {
            Ok(Ok(t)) if (200..300).contains(&t.status) => return Ok(t.status),
            Ok(Ok(t)) if (400..500).contains(&t.status) && t.status != 429 => {
                return Err(format!("HTTP {}", t.status));
            }
            Ok(Ok(t)) => last = format!("HTTP {}", t.status),
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "timed out".into(),
        }
    }
    Err(last)
}

impl Notifier {
    pub fn new(routes: Vec<Route>) -> Notifier {
        if routes.is_empty() {
            return Notifier {
                tx: None,
                worker: None,
            };
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<(Message, Vec<String>)>();
        let worker = tokio::spawn(async move {
            while let Some((message, tags)) = rx.recv().await {
                for route in routes.iter().filter(|r| wants(r, &tags)) {
                    if let Err(e) = deliver(route, &message).await {
                        eprintln!("notify {} {} failed: {e}", route.kind, route.url);
                    }
                }
            }
        });
        Notifier {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    pub fn send_tagged(&self, message: Message, tags: Vec<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send((message, tags));
        }
    }

    pub async fn finish(mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = tokio::time::timeout(Duration::from_secs(30), worker).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn message(event: &str) -> Message {
        Message {
            target: "dns/one".into(),
            alert: "loss".into(),
            event: event.into(),
            at_ms: 1_700_000_000_000,
            comment: Some("half \"lost\"".into()),
            loss: 0.5,
            median_ms: Some(12.25),
        }
    }

    fn route(kind: &str, url: String) -> Route {
        Route {
            kind: kind.into(),
            url,
            tags: Vec::new(),
            token: Some("s3cret".into()),
        }
    }

    async fn server(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for status in statuses.into_iter().cycle() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        (format!("http://{address}/hook"), seen)
    }

    #[tokio::test]
    async fn webhook_posts_json() {
        let (url, seen) = server(vec![200]).await;
        let status = deliver(&route("webhook", url), &message("raised"))
            .await
            .unwrap();
        assert_eq!(status, 200);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /hook HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains("Authorization: Bearer s3cret\r\n"));
        let body = &request[request.find("\r\n\r\n").unwrap() + 4..];
        assert_eq!(
            body,
            r#"{"source":"haze","target":"dns/one","alert":"loss","event":"raised","at_ms":1700000000000,"text":"dns/one loss raised: half \"lost\". 12.2 ms median, 50% loss in the last step","comment":"half \"lost\"","loss":0.5,"median_ms":12.25}"#
        );
    }

    #[tokio::test]
    async fn ntfy_posts_a_titled_message() {
        let (url, seen) = server(vec![200]).await;
        deliver(&route("ntfy", url), &message("cleared"))
            .await
            .unwrap();
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.contains("Title: dns/one loss cleared\r\n"));
        assert!(request.contains("Priority: default\r\n"));
        assert!(request.contains("Tags: white_check_mark\r\n"));
        assert!(
            request.ends_with("\r\n\r\nhalf \"lost\". 12.2 ms median, 50% loss in the last step")
        );
    }

    #[tokio::test]
    async fn server_errors_are_retried_and_client_errors_are_not() {
        let (url, seen) = server(vec![503, 200]).await;
        assert_eq!(
            deliver(&route("ntfy", url), &message("raised")).await,
            Ok(200)
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
        let (url, seen) = server(vec![404]).await;
        assert_eq!(
            deliver(&route("webhook", url), &message("raised")).await,
            Err("HTTP 404".into())
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn routes_filter_by_tag_and_the_notifier_drains_on_finish() {
        let (url, seen) = server(vec![200]).await;
        let mut dns_only = route("webhook", url);
        dns_only.tags = vec!["dns".into()];
        assert!(wants(&dns_only, &["dns".into(), "anycast".into()]));
        assert!(!wants(&dns_only, &["web".into()]));
        let notifier = Notifier::new(vec![dns_only]);
        notifier.send_tagged(message("raised"), vec!["web".into()]);
        notifier.send_tagged(message("raised"), vec!["dns".into()]);
        notifier.finish().await;
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
