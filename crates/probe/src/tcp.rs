use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pingwatch_core::{Probe, ProbeError, ProbeFuture, Reply};
use tokio::net::{TcpSocket, TcpStream};

use crate::net::SocketOptions;

pub struct TcpConnect {
    target: SocketAddr,
    source: Option<std::net::IpAddr>,
    tos: Option<u8>,
}

impl TcpConnect {
    pub fn new(target: SocketAddr, options: &SocketOptions) -> TcpConnect {
        TcpConnect {
            target,
            source: options.source,
            tos: options.tos,
        }
    }
}

pub async fn connect(
    target: SocketAddr,
    source: Option<std::net::IpAddr>,
    tos: Option<u8>,
) -> std::io::Result<TcpStream> {
    let socket = if target.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    if let Some(source) = source {
        socket.bind(SocketAddr::new(source, 0))?;
    }
    if let Some(tos) = tos
        && target.is_ipv4()
    {
        socket2::SockRef::from(&socket).set_tos_v4(tos as u32)?;
    }
    socket.set_nodelay(true)?;
    socket.connect(target).await
}

pub fn failure(e: std::io::Error) -> ProbeError {
    ProbeError::Failed(e.to_string())
}

impl Probe for TcpConnect {
    fn probe(&self, _seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            let started = Instant::now();
            let stream = tokio::time::timeout(timeout, connect(self.target, self.source, self.tos))
                .await
                .map_err(|_| ProbeError::Timeout)?
                .map_err(failure)?;
            let rtt = started.elapsed();
            drop(stream);
            Ok(Reply::rtt(rtt))
        })
    }
}
