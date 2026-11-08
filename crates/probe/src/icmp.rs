use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use pingwatch_core::{Probe, ProbeError, ProbeFuture, Reply};
use socket2::{Protocol, SockAddr, Socket};
use tokio::io::unix::AsyncFd;
use tokio::task::JoinHandle;

use crate::correlate::{Arrival, Correlator, random_u64};
use crate::net::{SocketOptions, datagram, recv_stamped};

const ECHO_REQUEST_V4: u8 = 8;
const ECHO_REPLY_V4: u8 = 0;
const ECHO_REQUEST_V6: u8 = 128;
const ECHO_REPLY_V6: u8 = 129;
const HEADER: usize = 8;
const TOKEN: usize = 8;

pub struct Icmp {
    socket: Arc<AsyncFd<Socket>>,
    target: SockAddr,
    v6: bool,
    identifier: u16,
    token: [u8; TOKEN],
    size: usize,
    correlator: Arc<Correlator>,
    receiver: JoinHandle<()>,
}

impl Drop for Icmp {
    fn drop(&mut self) {
        self.receiver.abort();
    }
}

pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in data.chunks(2) {
        let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
        sum += word as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn echo_request(
    v6: bool,
    identifier: u16,
    seq: u16,
    token: &[u8; TOKEN],
    size: usize,
) -> Vec<u8> {
    let mut packet = vec![0u8; HEADER + size.max(TOKEN)];
    packet[0] = if v6 { ECHO_REQUEST_V6 } else { ECHO_REQUEST_V4 };
    packet[4..6].copy_from_slice(&identifier.to_be_bytes());
    packet[6..8].copy_from_slice(&seq.to_be_bytes());
    packet[HEADER..HEADER + TOKEN].copy_from_slice(token);
    for (i, byte) in packet[HEADER + TOKEN..].iter_mut().enumerate() {
        *byte = i as u8;
    }
    if !v6 {
        let sum = checksum(&packet);
        packet[2..4].copy_from_slice(&sum.to_be_bytes());
    }
    packet
}

pub fn parse_reply(v6: bool, token: &[u8; TOKEN], data: &[u8]) -> Option<u16> {
    let icmp = if !v6 && data.first().is_some_and(|b| b >> 4 == 4) {
        let header = ((data[0] & 0x0f) as usize) * 4;
        data.get(header..)?
    } else {
        data
    };
    let expected = if v6 { ECHO_REPLY_V6 } else { ECHO_REPLY_V4 };
    if icmp.len() < HEADER + TOKEN || icmp[0] != expected || icmp[1] != 0 {
        return None;
    }
    if &icmp[HEADER..HEADER + TOKEN] != token {
        return None;
    }
    Some(u16::from_be_bytes([icmp[6], icmp[7]]))
}

pub fn unprivileged_ping_allowed() -> Result<(), String> {
    match Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(Protocol::ICMPV4),
    ) {
        Ok(_) => Ok(()),
        Err(e) => {
            let range = std::fs::read_to_string("/proc/sys/net/ipv4/ping_group_range")
                .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|_| "unknown".into());
            Err(format!(
                "unprivileged ICMP sockets are not allowed ({e}); net.ipv4.ping_group_range is `{range}`, set it to include your group, for example `sysctl net.ipv4.ping_group_range=\"0 2147483647\"`"
            ))
        }
    }
}

impl Icmp {
    pub fn new(target: IpAddr, size: usize, options: &SocketOptions) -> io::Result<Icmp> {
        let v6 = target.is_ipv6();
        let protocol = if v6 {
            Protocol::ICMPV6
        } else {
            Protocol::ICMPV4
        };
        let socket = datagram(target, Some(protocol), options).map_err(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    unprivileged_ping_allowed().err().unwrap_or(e.to_string()),
                )
            } else {
                e
            }
        })?;
        let socket = Arc::new(unsafe { AsyncFd::register(socket) }.map_err(|e| e.into_parts().1)?);
        let token = random_u64().to_be_bytes();
        let correlator = Arc::new(Correlator::default());
        let receiver = tokio::spawn(receive(socket.clone(), v6, token, correlator.clone()));
        Ok(Icmp {
            socket,
            target: SocketAddr::new(target, 0).into(),
            v6,
            identifier: random_u64() as u16,
            token,
            size,
            correlator,
            receiver,
        })
    }
}

async fn receive(
    socket: Arc<AsyncFd<Socket>>,
    v6: bool,
    token: [u8; TOKEN],
    correlator: Arc<Correlator>,
) {
    let mut buf = [0u8; 65536];
    loop {
        let Ok(mut guard) = socket.readable().await else {
            return;
        };
        match guard.try_io(|s| recv_stamped(s.get_ref(), &mut buf)) {
            Ok(Ok((n, kernel))) => {
                let at = Instant::now();
                if let Some(seq) = parse_reply(v6, &token, &buf[..n]) {
                    correlator.arrived(seq, Arrival { at, kernel });
                }
            }
            Ok(Err(_)) | Err(_) => continue,
        }
    }
}

impl Probe for Icmp {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            let packet = echo_request(self.v6, self.identifier, seq, &self.token, self.size);
            let rx = self.correlator.expect(seq);
            let sent_wall = SystemTime::now();
            let sent = Instant::now();
            loop {
                let mut guard = self
                    .socket
                    .writable()
                    .await
                    .map_err(|e| ProbeError::Failed(e.to_string()))?;
                match guard.try_io(|s| s.get_ref().send_to(&packet, &self.target)) {
                    Ok(Ok(_)) => break,
                    Ok(Err(e)) => {
                        self.correlator.forget(seq);
                        return Err(ProbeError::Failed(e.to_string()));
                    }
                    Err(_) => continue,
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_a_request_verifies_to_zero() {
        let packet = echo_request(false, 0x1234, 7, &[1, 2, 3, 4, 5, 6, 7, 8], 56);
        assert_eq!(packet.len(), 64);
        assert_eq!(checksum(&packet), 0);
    }

    #[test]
    fn replies_are_matched_by_token_with_or_without_an_ip_header() {
        let token = [9u8; TOKEN];
        let mut reply = echo_request(false, 1, 42, &token, 16);
        reply[0] = ECHO_REPLY_V4;
        assert_eq!(parse_reply(false, &token, &reply), Some(42));
        let mut with_header = vec![0x45u8; 20];
        with_header.extend_from_slice(&reply);
        assert_eq!(parse_reply(false, &token, &with_header), Some(42));
        assert_eq!(parse_reply(false, &[0u8; TOKEN], &reply), None);
        let request = echo_request(false, 1, 42, &token, 16);
        assert_eq!(parse_reply(false, &token, &request), None);
        let mut v6 = echo_request(true, 1, 3, &token, 16);
        v6[0] = ECHO_REPLY_V6;
        assert_eq!(parse_reply(true, &token, &v6), Some(3));
    }
}
