use std::net::SocketAddr;
use std::time::{Duration, Instant};

use haze_core::{Probe, ProbeError, ProbeFuture, Reply};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::net::SocketOptions;
use crate::tcp::{close_gently, connect, failure};

const LINE_LIMIT: usize = 64;

pub struct Smtp {
    target: SocketAddr,
    source: Option<std::net::IpAddr>,
    tos: Option<u8>,
}

impl Smtp {
    pub fn new(target: SocketAddr, options: &SocketOptions) -> Smtp {
        Smtp {
            target,
            source: options.source,
            tos: options.tos,
        }
    }

    async fn session(&self) -> Result<Reply, ProbeError> {
        let started = Instant::now();
        let stream = connect(self.target, self.source, self.tos)
            .await
            .map_err(failure)?;
        let connected = started.elapsed();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        let mut lines = 0;
        let last = loop {
            line.clear();
            if stream.read_line(&mut line).await.map_err(failure)? == 0 {
                return Err(ProbeError::Failed(
                    "the server closed the connection before a banner".into(),
                ));
            }
            lines += 1;
            let text = line.trim_end().to_string();
            if !text.bytes().take(3).all(|b| b.is_ascii_digit()) || text.len() < 3 {
                let _ = stream.get_mut().write_all(b"QUIT\r\n").await;
                close_gently(stream.into_inner());
                return Err(ProbeError::Failed(format!("not an SMTP banner: {text}")));
            }
            let continues = text.as_bytes().get(3) == Some(&b'-');
            if !continues || lines >= LINE_LIMIT {
                break text;
            }
        };
        let banner = started.elapsed();
        let _ = stream.get_mut().write_all(b"QUIT\r\n").await;
        close_gently(stream.into_inner());
        let code = last.get(..3).unwrap_or_default();
        let reply = Reply {
            rtt: banner,
            phases: vec![("connect", connected), ("banner", banner - connected)],
            note: Some(last.clone()),
        };
        match code {
            "220" => Ok(reply),
            "421" | "554" => Err(ProbeError::Reset(format!("SMTP {last}"))),
            _ => Err(ProbeError::Failed(format!("not an SMTP banner: {last}"))),
        }
    }
}

impl Probe for Smtp {
    fn probe(&self, _seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            tokio::time::timeout(timeout, self.session())
                .await
                .map_err(|_| ProbeError::Timeout)?
        })
    }
}
