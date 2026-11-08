use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pingwatch_core::{Probe, ProbeError, ProbeFuture, Reply};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::TlsConnector;

use crate::net::{SocketOptions, resolve};
use crate::tcp::{connect, failure};

const BODY_LIMIT: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl std::str::FromStr for Url {
    type Err = String;

    fn from_str(text: &str) -> Result<Url, String> {
        let (tls, rest) = if let Some(rest) = text.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = text.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(format!("`{text}` is not an http or https url"));
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let end = v6.find(']').ok_or_else(|| format!("`{text}` has an unclosed ["))?;
            let port = v6[end + 1..].strip_prefix(':');
            (v6[..end].to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => p.parse().map_err(|_| format!("`{p}` is not a port"))?,
            None if tls => 443,
            None => 80,
        };
        if host.is_empty() {
            return Err(format!("`{text}` has no host"));
        }
        Ok(Url {
            tls,
            host,
            port,
            path: path.to_string(),
        })
    }
}

pub struct Http {
    url: Url,
    family: Option<u8>,
    source: Option<IpAddr>,
    tos: Option<u8>,
    tls: TlsConnector,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpTiming {
    pub status: u16,
    pub dns: Duration,
    pub connect: Duration,
    pub tls: Option<Duration>,
    pub ttfb: Duration,
    pub total: Duration,
    pub cert_days_left: Option<i64>,
    pub bytes: usize,
}

pub fn tls_connector() -> TlsConnector {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

impl Http {
    pub fn new(url: &str, family: Option<u8>, options: &SocketOptions) -> Result<Http, String> {
        Ok(Http {
            url: url.parse()?,
            family,
            source: options.source,
            tos: options.tos,
            tls: tls_connector(),
        })
    }

    pub async fn fetch(&self) -> Result<HttpTiming, ProbeError> {
        let started = Instant::now();
        let ip = resolve(&self.url.host, self.family).await.map_err(failure)?;
        let dns = started.elapsed();
        let stream = connect(SocketAddr::new(ip, self.url.port), self.source, self.tos)
            .await
            .map_err(failure)?;
        let connect = started.elapsed();
        if !self.url.tls {
            return self.exchange(stream, started, dns, connect, None, None).await;
        }
        let name = ServerName::try_from(self.url.host.clone())
            .map_err(|e| ProbeError::Failed(format!("bad server name: {e}")))?;
        let stream = self
            .tls
            .connect(name, stream)
            .await
            .map_err(|e| ProbeError::Failed(format!("TLS: {e}")))?;
        let tls = started.elapsed() - connect;
        let days = stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|certs| certs.first())
            .and_then(|cert| cert_days_left(cert.as_ref()));
        self.exchange(stream, started, dns, connect, Some(tls), days).await
    }

    async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        mut stream: S,
        started: Instant,
        dns: Duration,
        connect: Duration,
        tls: Option<Duration>,
        cert_days_left: Option<i64>,
    ) -> Result<HttpTiming, ProbeError> {
        let host = if self.url.host.contains(':') {
            format!("[{}]", self.url.host)
        } else {
            self.url.host.clone()
        };
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pingwatch/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            self.url.path,
            env!("CARGO_PKG_VERSION"),
        );
        stream.write_all(request.as_bytes()).await.map_err(failure)?;
        let mut body = Vec::with_capacity(16 * 1024);
        let mut buf = [0u8; 16 * 1024];
        let first = stream.read(&mut buf).await.map_err(failure)?;
        let ttfb = started.elapsed();
        if first == 0 {
            return Err(ProbeError::Failed("connection closed before a response".into()));
        }
        body.extend_from_slice(&buf[..first]);
        let status = parse_status(&body).ok_or_else(|| ProbeError::Failed("not an HTTP response".into()))?;
        while body.len() < BODY_LIMIT && !response_complete(&body) {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(failure(e)),
            }
        }
        Ok(HttpTiming {
            status,
            dns,
            connect: connect - dns,
            tls,
            ttfb,
            total: started.elapsed(),
            cert_days_left,
            bytes: body.len(),
        })
    }
}

pub fn parse_status(response: &[u8]) -> Option<u16> {
    let line = response.split(|b| *b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split_whitespace();
    parts.next().filter(|v| v.starts_with("HTTP/"))?;
    parts.next()?.parse().ok()
}

fn response_complete(response: &[u8]) -> bool {
    let Some(end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&response[..end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    match length {
        Some(n) => response.len() >= end + 4 + n,
        None if head.contains("transfer-encoding: chunked") => response.ends_with(b"0\r\n\r\n"),
        None => false,
    }
}

fn cert_days_left(der: &[u8]) -> Option<i64> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let not_after = cert.validity().not_after.timestamp();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((not_after - now).div_euclid(86400))
}

impl Probe for Http {
    fn probe(&self, _seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            let timing = tokio::time::timeout(timeout, self.fetch())
                .await
                .map_err(|_| ProbeError::Timeout)??;
            if !(200..400).contains(&timing.status) {
                return Err(ProbeError::Failed(format!("HTTP {}", timing.status)));
            }
            let mut phases = vec![("dns", timing.dns), ("connect", timing.connect)];
            if let Some(tls) = timing.tls {
                phases.push(("tls", tls));
            }
            phases.push(("ttfb", timing.ttfb));
            let mut note = format!("HTTP {}", timing.status);
            if let Some(days) = timing.cert_days_left {
                note.push_str(&format!(", certificate expires in {days} days"));
            }
            Ok(Reply {
                rtt: timing.total,
                phases,
                note: Some(note),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let url: Url = "https://example.com/".parse().unwrap();
        assert_eq!((url.tls, url.host.as_str(), url.port, url.path.as_str()), (true, "example.com", 443, "/"));
        let url: Url = "http://127.0.0.1:8080/health?x=1".parse().unwrap();
        assert_eq!((url.port, url.path.as_str()), (8080, "/health?x=1"));
        let url: Url = "http://[::1]:81".parse().unwrap();
        assert_eq!((url.host.as_str(), url.port, url.path.as_str()), ("::1", 81, "/"));
        assert!("ftp://x".parse::<Url>().is_err());
        assert!("http://:80/".parse::<Url>().is_err());
    }

    #[test]
    fn reads_status_and_completion() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n\r\n"), Some(204));
        assert_eq!(parse_status(b"SSH-2.0-OpenSSH\r\n"), None);
        assert!(response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi"));
        assert!(!response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi"));
        assert!(response_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"));
    }
}
