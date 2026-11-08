use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::SystemTime;

use socket2::{Domain, Protocol, Socket, Type};

pub struct SocketOptions {
    pub source: Option<IpAddr>,
    pub tos: Option<u8>,
}

pub fn datagram(
    target: IpAddr,
    protocol: Option<Protocol>,
    options: &SocketOptions,
) -> io::Result<Socket> {
    let domain = if target.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, protocol)?;
    prepare(&socket, target, options)?;
    enable_timestamps(&socket);
    Ok(socket)
}

pub fn prepare(socket: &Socket, target: IpAddr, options: &SocketOptions) -> io::Result<()> {
    socket.set_nonblocking(true)?;
    if let Some(source) = options.source {
        if source.is_ipv4() != target.is_ipv4() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("source {source} is not the same family as {target}"),
            ));
        }
        socket.bind(&SocketAddr::new(source, 0).into())?;
    }
    if let Some(tos) = options.tos {
        if target.is_ipv4() {
            socket.set_tos_v4(tos as u32)?;
        } else {
            #[cfg(not(target_os = "windows"))]
            socket.set_tclass_v6(tos as u32)?;
        }
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn enable_timestamps(socket: &Socket) {
    use std::os::fd::AsRawFd;
    let on: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            &on as *const _ as *const libc::c_void,
            std::mem::size_of_val(&on) as libc::socklen_t,
        );
    }
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn enable_timestamps(_socket: &Socket) {}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn recv_stamped(socket: &Socket, buf: &mut [u8]) -> io::Result<(usize, Option<SystemTime>)> {
    use std::os::fd::AsRawFd;
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut control = [0u64; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut stamp = None;
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_TIMESTAMPNS
            {
                let ts: libc::timespec =
                    std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const libc::timespec);
                stamp = Some(
                    SystemTime::UNIX_EPOCH
                        + std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32),
                );
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok((n as usize, stamp))
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub fn recv_stamped(socket: &Socket, buf: &mut [u8]) -> io::Result<(usize, Option<SystemTime>)> {
    use std::io::Read;
    let mut reader = socket;
    reader.read(buf).map(|n| (n, None))
}

pub async fn resolve(host: &str, family: Option<u8>) -> io::Result<IpAddr> {
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, 0))
        .await?
        .map(|a| a.ip())
        .collect();
    let wanted = |ip: &&IpAddr| match family {
        Some(4) => ip.is_ipv4(),
        Some(6) => ip.is_ipv6(),
        _ => true,
    };
    addrs.iter().find(wanted).copied().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{host} has no {} address",
                family.map(|f| format!("IPv{f}")).unwrap_or_default()
            ),
        )
    })
}
