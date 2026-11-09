use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::{Duration, Instant};

use haze_core::{Probe, ProbeError, ProbeFuture, Reply};
use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::{Name, RecordType};
use tokio::net::UdpSocket;

use crate::correlate::random_u64;
use crate::net::{SocketOptions, prepare};

pub struct Dns {
    server: SocketAddr,
    name: Name,
    record: RecordType,
    options: SocketOptions,
}

pub fn record_type(text: &str) -> Result<RecordType, String> {
    RecordType::from_str(&text.to_ascii_uppercase())
        .map_err(|_| format!("`{text}` is not a record type"))
}

pub fn query(id: u16, name: &Name, record: RecordType) -> Result<Vec<u8>, String> {
    let mut message = Message::new(id, MessageType::Query, OpCode::Query);
    message.metadata.recursion_desired = true;
    message.add_query(Query::query(name.clone(), record));
    message.to_vec().map_err(|e| e.to_string())
}

#[derive(Debug, PartialEq)]
pub enum Answer {
    Mine { code: ResponseCode, answers: usize },
    Other,
}

pub fn check_answer(bytes: &[u8], id: u16, name: &Name, record: RecordType) -> Answer {
    let Ok(message) = Message::from_vec(bytes) else {
        return Answer::Other;
    };
    let same_question = message
        .queries
        .first()
        .is_some_and(|q| q.name() == name && q.query_type() == record);
    if message.metadata.id != id
        || message.metadata.message_type != MessageType::Response
        || !same_question
    {
        return Answer::Other;
    }
    Answer::Mine {
        code: message.metadata.response_code,
        answers: message.answers.len(),
    }
}

impl Dns {
    pub fn new(
        server: SocketAddr,
        name: &str,
        record: &str,
        options: SocketOptions,
    ) -> Result<Dns, String> {
        let mut name =
            Name::from_str(name).map_err(|e| format!("`{name}` is not a domain name: {e}"))?;
        name.set_fqdn(true);
        Ok(Dns {
            server,
            name,
            record: record_type(record)?,
            options,
        })
    }

    async fn socket(&self) -> io::Result<UdpSocket> {
        let domain = if self.server.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        };
        let raw = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        prepare(&raw, self.server.ip(), &self.options)?;
        if self.options.source.is_none() {
            let any: std::net::IpAddr = if self.server.is_ipv4() {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            };
            raw.bind(&SocketAddr::new(any, 0).into())?;
        }
        let socket = UdpSocket::from_std(raw.into())?;
        socket.connect(self.server).await?;
        Ok(socket)
    }

    pub async fn ask(&self) -> Result<Reply, ProbeError> {
        let socket = self
            .socket()
            .await
            .map_err(|e| ProbeError::Failed(e.to_string()))?;
        let id = random_u64() as u16;
        let packet = query(id, &self.name, self.record).map_err(ProbeError::Failed)?;
        let started = Instant::now();
        socket
            .send(&packet)
            .await
            .map_err(|e| ProbeError::Failed(e.to_string()))?;
        let mut buf = [0u8; 4096];
        loop {
            let n = socket
                .recv(&mut buf)
                .await
                .map_err(|e| ProbeError::Failed(e.to_string()))?;
            let rtt = started.elapsed();
            match check_answer(&buf[..n], id, &self.name, self.record) {
                Answer::Other => continue,
                Answer::Mine { code, answers } => {
                    if !matches!(code, ResponseCode::NoError | ResponseCode::NXDomain) {
                        return Err(ProbeError::Failed(format!("{code}")));
                    }
                    return Ok(Reply {
                        rtt,
                        phases: Vec::new(),
                        note: Some(format!("{code}, {answers} answers")),
                    });
                }
            }
        }
    }
}

impl Probe for Dns {
    fn probe(&self, _seq: u16, timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            tokio::time::timeout(timeout, self.ask())
                .await
                .map_err(|_| ProbeError::Timeout)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_must_match_the_id_and_question() {
        let name = Name::from_str("example.com.").unwrap();
        let other = Name::from_str("example.org.").unwrap();
        let request = query(7, &name, RecordType::A).unwrap();
        assert_eq!(
            check_answer(&request, 7, &name, RecordType::A),
            Answer::Other,
            "a query is not an answer"
        );
        let mut response = Message::from_vec(&request).unwrap();
        response.metadata.message_type = MessageType::Response;
        let bytes = response.to_vec().unwrap();
        assert_eq!(
            check_answer(&bytes, 7, &name, RecordType::A),
            Answer::Mine {
                code: ResponseCode::NoError,
                answers: 0
            }
        );
        assert_eq!(check_answer(&bytes, 8, &name, RecordType::A), Answer::Other);
        assert_eq!(
            check_answer(&bytes, 7, &other, RecordType::A),
            Answer::Other
        );
        assert_eq!(
            check_answer(&bytes, 7, &name, RecordType::AAAA),
            Answer::Other
        );
        assert_eq!(
            check_answer(b"junk", 7, &name, RecordType::A),
            Answer::Other
        );
    }

    #[test]
    fn record_types_parse() {
        assert_eq!(record_type("aaaa"), Ok(RecordType::AAAA));
        assert!(record_type("NOPE").is_err());
    }
}
