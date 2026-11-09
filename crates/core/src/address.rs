use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

pub fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
}

pub fn is_private_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => is_private_v6(v6),
    }
}

pub fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    if let Some(inner) = authority.strip_prefix('[') {
        return inner.split(']').next().unwrap_or_default();
    }
    authority.split(':').next().unwrap_or_default()
}

pub fn host_is_private<R>(host: &str, resolve: R) -> bool
where
    R: FnOnce(&str) -> Option<Vec<IpAddr>>,
{
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_private(ip);
    }
    match resolve(bare) {
        Some(ips) if !ips.is_empty() => ips.into_iter().all(is_private),
        _ => true,
    }
}

pub fn resolve_system(host: &str) -> Option<Vec<IpAddr>> {
    (host, 0)
        .to_socket_addrs()
        .ok()
        .map(|addrs| addrs.map(|a| a.ip()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(text: &str) -> bool {
        is_private(text.parse().unwrap())
    }

    #[test]
    fn private_ranges_v4() {
        for ip in [
            "10.97.0.5",
            "10.252.48.4",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.10.1",
            "100.64.0.1",
            "100.127.255.254",
            "0.0.0.0",
            "255.255.255.255",
        ] {
            assert!(private(ip), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "185.18.221.135",
            "172.32.0.1",
            "172.15.255.255",
            "100.63.255.255",
            "100.128.0.1",
            "192.169.0.1",
        ] {
            assert!(!private(ip), "{ip}");
        }
    }

    #[test]
    fn private_ranges_v6() {
        for ip in [
            "::1",
            "::",
            "fd48::4",
            "fd97::10",
            "fc00::1",
            "fe80::84bb:81ff:feaa:35ab",
            "febf::1",
            "fec0::1",
            "::ffff:10.97.0.10",
            "::ffff:192.168.0.1",
        ] {
            assert!(private(ip), "{ip}");
        }
        for ip in [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2a13:2c0::1",
            "fe00::1",
            "::ffff:1.1.1.1",
        ] {
            assert!(!private(ip), "{ip}");
        }
    }

    #[test]
    fn url_hosts() {
        assert_eq!(url_host("https://10.97.0.5:8006/api"), "10.97.0.5");
        assert_eq!(url_host("http://[fd97::5]:80/"), "fd97::5");
        assert_eq!(url_host("https://user:pw@lnvps.net/?x"), "lnvps.net");
        assert_eq!(url_host("https://lnvps.net#a"), "lnvps.net");
    }

    #[test]
    fn hostnames_are_private_only_when_every_address_is() {
        let fixed = |ips: &'static [&'static str]| {
            move |_: &str| Some(ips.iter().map(|i| i.parse().unwrap()).collect())
        };
        assert!(host_is_private("pve.lan", fixed(&["10.97.0.5", "fd97::5"])));
        assert!(!host_is_private("mixed", fixed(&["10.97.0.5", "1.1.1.1"])));
        assert!(!host_is_private("lnvps.net", fixed(&["185.18.221.135"])));
        assert!(host_is_private("nowhere.invalid", |_| None));
        assert!(host_is_private("empty", |_| Some(Vec::new())));
        assert!(host_is_private("[fd97::5]", |_| None));
        assert!(!host_is_private("1.1.1.1", |_| None));
        assert!(host_is_private("localhost", resolve_system));
    }
}
