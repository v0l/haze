use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use pingwatch_core::{Probe, ProbeError, ProbeFuture, Reply};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::correlate::{Arrival, Correlator, random_u64};
use crate::net::{SocketOptions, enable_timestamps, prepare, recv_stamped};

const TOKEN: usize = 8;

pub struct UdpEcho {
    socket: Arc<UdpSocket>,
    token: [u8; TOKEN],
    size: usize,
    correlator: Arc<Correlator>,
    receiver: JoinHandle<()>,
}

impl Drop for UdpEcho {
    fn drop(&mut self) {
        self.receiver.abort();
    }
}

impl UdpEcho {
    pub async fn new(
        target: SocketAddr,
        size: usize,
        options: &SocketOptions,
    ) -> io::Result<UdpEcho> {
        let domain = if target.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        };
        let raw = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        prepare(&raw, target.ip(), options)?;
        enable_timestamps(&raw);
        if options.source.is_none() {
            let any: IpAddr = if target.is_ipv4() {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            };
            raw.bind(&SocketAddr::new(any, 0).into())?;
        }
        let socket = UdpSocket::from_std(raw.into())?;
        socket.connect(target).await?;
        let socket = Arc::new(socket);
        let token = random_u64().to_be_bytes();
        let correlator = Arc::new(Correlator::default());
        let receiver = tokio::spawn(receive(socket.clone(), token, correlator.clone()));
        Ok(UdpEcho {
            socket,
            token,
            size,
            correlator,
            receiver,
        })
    }
}

async fn receive(socket: Arc<UdpSocket>, token: [u8; TOKEN], correlator: Arc<Correlator>) {
    let mut buf = [0u8; 65536];
    loop {
        let received = socket
            .async_io(tokio::io::Interest::READABLE, || {
                recv_stamped(&socket2::SockRef::from(&*socket), &mut buf)
            })
            .await;
        match received {
            Ok((n, kernel)) if n >= TOKEN + 2 && buf[..TOKEN] == token => {
                let seq = u16::from_be_bytes([buf[TOKEN], buf[TOKEN + 1]]);
                correlator.arrived(
                    seq,
                    Arrival {
                        at: Instant::now(),
                        kernel,
                    },
                );
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

impl Probe for UdpEcho {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            let mut packet = vec![0u8; self.size.max(TOKEN + 2)];
            packet[..TOKEN].copy_from_slice(&self.token);
            packet[TOKEN..TOKEN + 2].copy_from_slice(&seq.to_be_bytes());
            let rx = self.correlator.expect(seq);
            let sent_wall = SystemTime::now();
            let sent = Instant::now();
            if let Err(e) = self.socket.send(&packet).await {
                self.correlator.forget(seq);
                return Err(ProbeError::Failed(e.to_string()));
            }
            let rtt = self
                .correlator
                .wait(seq, rx, sent, sent_wall, timeout)
                .await?;
            Ok(Reply::rtt(rtt))
        })
    }

    fn duplicates(&self) -> u64 {
        self.correlator.duplicates()
    }
}
