use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::duration::parse_duration;
use crate::matcher::{AlertPolicy, Matcher};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub probe: Option<String>,
    pub step: Option<String>,
    pub pings: Option<u32>,
    pub timeout: Option<String>,
    pub family: Option<u8>,
    pub source: Option<String>,
    pub size: Option<usize>,
    pub tos: Option<u8>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub record: Option<String>,
    pub event: Option<String>,
    pub method: Option<String>,
    pub allow_aggressive: Option<bool>,
}

impl Settings {
    fn layered_over(&self, parent: &Settings) -> Settings {
        Settings {
            probe: self.probe.clone().or_else(|| parent.probe.clone()),
            step: self.step.clone().or_else(|| parent.step.clone()),
            pings: self.pings.or(parent.pings),
            timeout: self.timeout.clone().or_else(|| parent.timeout.clone()),
            family: self.family.or(parent.family),
            source: self.source.clone().or_else(|| parent.source.clone()),
            size: self.size.or(parent.size),
            tos: self.tos.or(parent.tos),
            port: self.port.or(parent.port),
            name: self.name.clone().or_else(|| parent.name.clone()),
            record: self.record.clone().or_else(|| parent.record.clone()),
            event: self.event.clone().or_else(|| parent.event.clone()),
            method: self.method.clone().or_else(|| parent.method.clone()),
            allow_aggressive: self.allow_aggressive.or(parent.allow_aggressive),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
    pub when: String,
    pub clear_after: Option<usize>,
    pub baseline_steps: Option<usize>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub dir: String,
    pub retention_days: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Web {
    pub listen: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NotifyRoute {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default)]
    pub defaults: Settings,
    pub storage: Option<Storage>,
    pub web: Option<Web>,
    #[serde(default)]
    pub notify: Vec<NotifyRoute>,
    #[serde(default)]
    pub alerts: BTreeMap<String, AlertRule>,
    #[serde(default)]
    pub targets: toml::Table,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeKind {
    Icmp,
    Tcp,
    Udp,
    Http,
    Nostr,
    Dns,
}

impl ProbeKind {
    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "icmp" | "ping" => ProbeKind::Icmp,
            "tcp" => ProbeKind::Tcp,
            "udp" => ProbeKind::Udp,
            "http" | "https" => ProbeKind::Http,
            "nostr" => ProbeKind::Nostr,
            "dns" => ProbeKind::Dns,
            other => {
                return Err(format!(
                    "`{other}` is not a probe (icmp, tcp, udp, http, nostr, dns)"
                ));
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            ProbeKind::Icmp => "icmp",
            ProbeKind::Tcp => "tcp",
            ProbeKind::Udp => "udp",
            ProbeKind::Http => "http",
            ProbeKind::Nostr => "nostr",
            ProbeKind::Dns => "dns",
        }
    }

    pub fn default_pings(self) -> u32 {
        match self {
            ProbeKind::Icmp | ProbeKind::Udp => MAX_PINGS,
            ProbeKind::Dns => 5,
            ProbeKind::Tcp => 3,
            ProbeKind::Http | ProbeKind::Nostr => 1,
        }
    }

    pub fn opens_connections(self) -> bool {
        matches!(self, ProbeKind::Tcp | ProbeKind::Http | ProbeKind::Nostr)
    }
}

#[derive(Clone, Debug)]
pub struct Alert {
    pub name: String,
    pub matcher: Matcher,
    pub policy: AlertPolicy,
    pub comment: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub path: String,
    pub title: Option<String>,
    pub host: Option<String>,
    pub url: Option<String>,
    pub probe: ProbeKind,
    pub step: Duration,
    pub pings: u32,
    pub timeout: Duration,
    pub family: Option<u8>,
    pub source: Option<String>,
    pub size: usize,
    pub tos: Option<u8>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub record: Option<String>,
    pub event: Option<String>,
    pub method: Option<String>,
    pub allow_aggressive: bool,
    pub tags: Vec<String>,
    pub alerts: Vec<String>,
    pub depends: Vec<String>,
}

impl Target {
    pub fn destination(&self) -> String {
        if let Some(url) = &self.url {
            let (secure, rest) = match url.split_once("://") {
                Some((scheme, rest)) => (matches!(scheme, "https" | "wss"), rest),
                None => (false, url.as_str()),
            };
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let has_port = match authority.rfind(']') {
                Some(end) => authority[end..].contains(':'),
                None => authority.contains(':'),
            };
            let port = if secure { 443 } else { 80 };
            return if has_port {
                authority.to_ascii_lowercase()
            } else {
                format!("{}:{port}", authority.to_ascii_lowercase())
            };
        }
        let host = self
            .host
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let port = self.port.or((self.probe == ProbeKind::Dns).then_some(53));
        match port {
            Some(port) if host.contains(':') => format!("[{host}]:{port}"),
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }

    pub fn per_minute(&self) -> f64 {
        self.pings as f64 * 60.0 / self.step.as_secs_f64()
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub storage_dir: String,
    pub retention_days: u32,
    pub listen: Option<String>,
    pub notify: Vec<NotifyRoute>,
    pub alerts: BTreeMap<String, Alert>,
    pub targets: Vec<Target>,
}

pub const MIN_STEP: Duration = Duration::from_secs(5);
pub const MAX_PINGS: u32 = 20;
pub const POLITE_CONNECTIONS_PER_MINUTE: f64 = 6.0;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    title: Option<String>,
    host: Option<String>,
    url: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    alerts: Vec<String>,
    #[serde(default)]
    depends: Vec<String>,
    #[serde(flatten)]
    settings: Settings,
}

struct Inherited {
    settings: Settings,
    tags: Vec<String>,
    alerts: Vec<String>,
    depends: Vec<String>,
}

fn add_unique(into: &mut Vec<String>, more: &[String]) {
    for item in more {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut alerts = BTreeMap::new();
        for (name, rule) in &raw.alerts {
            let mut matcher: Matcher = rule
                .when
                .parse()
                .map_err(|e| format!("alert `{name}`: {e}"))?;
            if let Some(steps) = rule.baseline_steps {
                matcher = matcher.with_baseline_steps(steps);
            }
            let mut policy = AlertPolicy::default();
            if let Some(n) = rule.clear_after {
                policy.clear_after = n.max(1);
            }
            alerts.insert(
                name.clone(),
                Alert {
                    name: name.clone(),
                    matcher,
                    policy,
                    comment: rule.comment.clone(),
                },
            );
        }
        let root = Inherited {
            settings: raw.defaults.clone(),
            tags: Vec::new(),
            alerts: Vec::new(),
            depends: Vec::new(),
        };
        let mut targets = Vec::new();
        walk(&raw.targets, "", &root, &mut targets)?;
        check_politeness(&targets)?;
        for target in &targets {
            for alert in &target.alerts {
                if !alerts.contains_key(alert) {
                    return Err(format!(
                        "target `{}` uses unknown alert `{alert}`",
                        target.path
                    ));
                }
            }
            for dep in &target.depends {
                if !targets.iter().any(|t| &t.path == dep) {
                    return Err(format!(
                        "target `{}` depends on unknown target `{dep}`",
                        target.path
                    ));
                }
            }
        }
        let storage = raw.storage.unwrap_or(Storage {
            dir: "data".into(),
            retention_days: None,
        });
        for route in &raw.notify {
            if !matches!(route.kind.as_str(), "webhook" | "ntfy") {
                return Err(format!(
                    "notify kind `{}` is not webhook or ntfy",
                    route.kind
                ));
            }
            if !(route.url.starts_with("http://") || route.url.starts_with("https://")) {
                return Err(format!("notify url `{}` is not http or https", route.url));
            }
        }
        Ok(Config {
            storage_dir: storage.dir,
            retention_days: storage.retention_days.unwrap_or(400),
            listen: raw.web.and_then(|w| w.listen),
            notify: raw.notify,
            alerts,
            targets,
        })
    }

    pub fn target(&self, path: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.path == path)
    }
}

const NODE_KEYS: [&str; 20] = [
    "title",
    "host",
    "url",
    "tags",
    "alerts",
    "depends",
    "probe",
    "step",
    "pings",
    "timeout",
    "family",
    "source",
    "size",
    "tos",
    "port",
    "name",
    "record",
    "event",
    "method",
    "allow_aggressive",
];

fn check_politeness(targets: &[Target]) -> Result<(), String> {
    let mut per_host: BTreeMap<String, f64> = BTreeMap::new();
    for t in targets.iter().filter(|t| t.probe.opens_connections()) {
        *per_host.entry(t.destination()).or_default() += t.per_minute();
    }
    for t in targets.iter().filter(|t| t.probe.opens_connections()) {
        let rate = per_host[&t.destination()];
        if !t.allow_aggressive && rate > POLITE_CONNECTIONS_PER_MINUTE + 1e-9 {
            return Err(format!(
                "target `{}`: {} would get {} connections a minute from haze, over the {} that \
                 rate limits and auto-bans tolerate; lower pings or raise step, or set \
                 allow_aggressive = true on the target",
                t.path,
                t.destination(),
                rate.round(),
                POLITE_CONNECTIONS_PER_MINUTE,
            ));
        }
    }
    Ok(())
}

fn walk(
    table: &toml::Table,
    prefix: &str,
    parent: &Inherited,
    out: &mut Vec<Target>,
) -> Result<(), String> {
    for (name, value) in table {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let toml::Value::Table(child) = value else {
            return Err(format!("`{path}` is not a table of a target or group"));
        };
        let (own, nested): (toml::Table, toml::Table) = child
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .partition(|(k, v)| NODE_KEYS.contains(&k.as_str()) || !v.is_table());
        let node: Node = own
            .try_into()
            .map_err(|e: toml::de::Error| format!("`{path}`: {}", e.message()))?;
        let mut inherited = Inherited {
            settings: node.settings.layered_over(&parent.settings),
            tags: parent.tags.clone(),
            alerts: parent.alerts.clone(),
            depends: parent.depends.clone(),
        };
        add_unique(&mut inherited.tags, &node.tags);
        add_unique(&mut inherited.alerts, &node.alerts);
        add_unique(&mut inherited.depends, &node.depends);
        if node.host.is_some() || node.url.is_some() {
            if !nested.is_empty() {
                return Err(format!("target `{path}` cannot also hold targets"));
            }
            out.push(resolve(path, node, inherited)?);
        } else if nested.is_empty() {
            return Err(format!(
                "`{path}` has no host or url and no targets under it"
            ));
        } else {
            walk(&nested, &path, &inherited, out)?;
        }
    }
    Ok(())
}

fn resolve(path: String, node: Node, inherited: Inherited) -> Result<Target, String> {
    let s = inherited.settings;
    let probe = match (&s.probe, &node.url) {
        (Some(p), _) => ProbeKind::parse(p)?,
        (None, Some(url)) if url.starts_with("ws://") || url.starts_with("wss://") => {
            ProbeKind::Nostr
        }
        (None, Some(_)) => ProbeKind::Http,
        (None, None) => ProbeKind::Icmp,
    };
    let duration =
        |field: &str, value: &Option<String>, default: Duration| -> Result<Duration, String> {
            value
                .as_deref()
                .map(parse_duration)
                .transpose()
                .map(|d| d.unwrap_or(default))
                .map_err(|e| format!("target `{path}` {field}: {e}"))
        };
    let step = duration("step", &s.step, Duration::from_secs(60))?;
    let timeout = duration("timeout", &s.timeout, Duration::from_secs(2))?;
    let pings = s.pings.unwrap_or(probe.default_pings());
    if step < MIN_STEP {
        return Err(format!(
            "target `{path}`: step must be at least {}s",
            MIN_STEP.as_secs()
        ));
    }
    if pings == 0 || pings > MAX_PINGS {
        return Err(format!("target `{path}`: pings must be 1 to {MAX_PINGS}"));
    }
    if timeout >= step {
        return Err(format!(
            "target `{path}`: timeout must be shorter than the step"
        ));
    }
    if let Some(f) = s.family
        && f != 4
        && f != 6
    {
        return Err(format!("target `{path}`: family is 4 or 6, not {f}"));
    }
    let needs_url = matches!(probe, ProbeKind::Http | ProbeKind::Nostr);
    if needs_url && node.url.is_none() {
        return Err(format!(
            "target `{path}`: the {} probe needs a url",
            probe.name()
        ));
    }
    if !needs_url && node.host.is_none() {
        return Err(format!(
            "target `{path}`: the {} probe needs a host",
            probe.name()
        ));
    }
    if probe == ProbeKind::Nostr
        && let Some(url) = &node.url
        && !(url.starts_with("ws://") || url.starts_with("wss://"))
    {
        return Err(format!(
            "target `{path}`: a nostr relay url starts with wss:// or ws://"
        ));
    }
    if let Some(event) = &s.event
        && (event.len() != 64 || !event.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!(
            "target `{path}`: event must be a 64 character hex event id"
        ));
    }
    if probe == ProbeKind::Dns && s.name.is_none() {
        return Err(format!(
            "target `{path}`: the dns probe needs a name to look up"
        ));
    }
    if let Some(record) = &s.record
        && !matches!(
            record.to_ascii_uppercase().as_str(),
            "A" | "AAAA" | "NS" | "MX" | "TXT" | "SOA" | "CNAME"
        )
    {
        return Err(format!(
            "target `{path}`: record `{record}` is not A, AAAA, NS, MX, TXT, SOA or CNAME"
        ));
    }
    if let Some(method) = &s.method
        && !matches!(method.to_ascii_uppercase().as_str(), "HEAD" | "GET")
    {
        return Err(format!(
            "target `{path}`: method `{method}` is not HEAD or GET"
        ));
    }
    if matches!(probe, ProbeKind::Tcp | ProbeKind::Udp) && s.port.is_none() {
        return Err(format!(
            "target `{path}`: the {} probe needs a port",
            probe.name()
        ));
    }
    Ok(Target {
        path,
        title: node.title,
        host: node.host,
        url: node.url,
        probe,
        step,
        pings,
        timeout,
        family: s.family,
        source: s.source,
        size: s.size.unwrap_or(56),
        tos: s.tos,
        port: s.port,
        name: s.name,
        record: s.record.map(|r| r.to_ascii_uppercase()),
        event: s.event.map(|e| e.to_ascii_lowercase()),
        method: s.method.map(|m| m.to_ascii_uppercase()),
        allow_aggressive: s.allow_aggressive.unwrap_or(false),
        tags: inherited.tags,
        alerts: inherited.alerts,
        depends: inherited.depends,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[defaults]
step = "60s"
timeout = "2s"

[storage]
dir = "/var/lib/haze"

[alerts.loss]
when = "loss >= 10% for 3 steps"
clear_after = 2

[alerts.slow]
when = "median > 2x baseline 3 of 5"
baseline_steps = 60

[targets.dns]
tags = ["dns"]
alerts = ["loss"]
step = "30s"

[targets.dns.cloudflare]
host = "1.1.1.1"
tags = ["anycast"]

[targets.dns.google]
host = "8.8.8.8"
pings = 10
alerts = ["slow"]
depends = ["edge/router"]

[targets.dns.v6.cloudflare]
host = "2606:4700:4700::1111"
family = 6

[targets.edge.router]
host = "192.0.2.1"

[targets.web.example]
url = "https://example.com/"
timeout = "5s"

[targets.web.ssh]
host = "example.com"
probe = "tcp"
port = 22
"#;

    #[test]
    fn targets_inherit_from_their_groups() {
        let config = Config::parse(EXAMPLE).unwrap();
        let paths: Vec<&str> = config.targets.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "dns/cloudflare",
                "dns/google",
                "dns/v6/cloudflare",
                "edge/router",
                "web/example",
                "web/ssh"
            ]
        );
        let cf = config.target("dns/cloudflare").unwrap();
        assert_eq!(cf.step, Duration::from_secs(30));
        assert_eq!(cf.pings, 20);
        assert_eq!(cf.timeout, Duration::from_secs(2));
        assert_eq!(cf.tags, ["dns", "anycast"]);
        assert_eq!(cf.alerts, ["loss"]);
        assert_eq!(cf.probe, ProbeKind::Icmp);

        let google = config.target("dns/google").unwrap();
        assert_eq!(google.pings, 10);
        assert_eq!(google.alerts, ["loss", "slow"]);
        assert_eq!(google.depends, ["edge/router"]);

        let v6 = config.target("dns/v6/cloudflare").unwrap();
        assert_eq!(v6.family, Some(6));
        assert_eq!(v6.step, Duration::from_secs(30));
        assert_eq!(v6.tags, ["dns"]);

        let router = config.target("edge/router").unwrap();
        assert_eq!(router.step, Duration::from_secs(60));
        assert!(router.alerts.is_empty());

        let web = config.target("web/example").unwrap();
        assert_eq!(web.probe, ProbeKind::Http);
        assert_eq!(web.timeout, Duration::from_secs(5));
        assert_eq!(config.target("web/ssh").unwrap().port, Some(22));

        assert_eq!(config.storage_dir, "/var/lib/haze");
        assert_eq!(config.retention_days, 400);
        assert_eq!(config.alerts["loss"].policy.clear_after, 2);
        assert!(matches!(
            config.alerts["slow"].matcher,
            Matcher::Threshold {
                baseline_steps: 60,
                ..
            }
        ));
    }

    fn error_of(text: &str) -> String {
        Config::parse(text).expect_err("should not parse")
    }

    #[test]
    fn dns_and_nostr_settings_are_inherited_from_groups() {
        let config = Config::parse(
            "[targets.dns]\nprobe = \"dns\"\nname = \"example.com\"\nrecord = \"aaaa\"\n[targets.dns.cloudflare]\nhost = \"1.1.1.1\"\n[targets.dns.google]\nhost = \"8.8.8.8\"\nname = \"example.org\"",
        )
        .unwrap();
        let cf = config.target("dns/cloudflare").unwrap();
        assert_eq!(cf.probe, ProbeKind::Dns);
        assert_eq!(cf.name.as_deref(), Some("example.com"));
        assert_eq!(cf.record.as_deref(), Some("AAAA"));
        let google = config.target("dns/google").unwrap();
        assert_eq!(google.name.as_deref(), Some("example.org"));
        assert_eq!(google.record.as_deref(), Some("AAAA"));
    }

    #[test]
    fn web_notify_nostr_and_dns_settings() {
        let config = Config::parse(
            r#"
[web]
listen = "127.0.0.1:8480"

[[notify]]
kind = "ntfy"
url = "https://ntfy.sh/haze-alerts"
tags = ["dns"]

[[notify]]
kind = "webhook"
url = "http://127.0.0.1:9000/hook"

[targets.relay]
url = "wss://relay.snort.social"
event = "D7DD5EB3AB747E16F8D0212D53032EA2A7CADEF53837E5A6C66D42849FCB9027"

[targets.lookup]
probe = "dns"
host = "1.1.1.1"
name = "example.com"
record = "aaaa"
"#,
        )
        .unwrap();
        assert_eq!(config.listen.as_deref(), Some("127.0.0.1:8480"));
        assert_eq!(config.notify.len(), 2);
        assert_eq!(config.notify[0].tags, ["dns"]);
        let relay = config.target("relay").unwrap();
        assert_eq!(relay.probe, ProbeKind::Nostr);
        assert_eq!(
            relay.event.as_deref(),
            Some("d7dd5eb3ab747e16f8d0212d53032ea2a7cadef53837e5a6c66d42849fcb9027")
        );
        let lookup = config.target("lookup").unwrap();
        assert_eq!(lookup.probe, ProbeKind::Dns);
        assert_eq!(lookup.record.as_deref(), Some("AAAA"));
        assert!(
            error_of("[targets.a]\nprobe = \"dns\"\nhost = \"1.1.1.1\"").contains("needs a name")
        );
        assert!(error_of("[targets.a]\nprobe = \"nostr\"\nurl = \"https://x\"").contains("wss://"));
        assert!(
            error_of("[targets.a]\nurl = \"wss://x\"\nevent = \"abc\"").contains("64 character")
        );
        assert!(
            error_of("[targets.a]\nprobe = \"dns\"\nhost = \"x\"\nname = \"y\"\nrecord = \"PTRX\"")
                .contains("record")
        );
        assert!(
            error_of("[[notify]]\nkind = \"pager\"\nurl = \"http://x\"")
                .contains("webhook or ntfy")
        );
        assert!(
            error_of("[[notify]]\nkind = \"ntfy\"\nurl = \"ftp://x\"").contains("http or https")
        );
    }

    #[test]
    fn pings_default_per_probe_kind() {
        let config = Config::parse(
            r#"
[targets.icmp]
host = "1.1.1.1"
[targets.udp]
host = "127.0.0.1"
probe = "udp"
port = 7
[targets.dns]
host = "1.1.1.1"
probe = "dns"
name = "example.com"
[targets.tcp]
host = "example.com"
probe = "tcp"
port = 443
[targets.http]
url = "https://example.com/"
[targets.nostr]
url = "wss://relay.snort.social"
[targets.set]
url = "https://example.org/"
pings = 3
step = "30s"
"#,
        )
        .unwrap();
        let pings = |p: &str| {
            let t = config.target(p).unwrap();
            (t.pings, t.step.as_secs())
        };
        assert_eq!(pings("icmp"), (20, 60));
        assert_eq!(pings("udp"), (20, 60));
        assert_eq!(pings("dns"), (5, 60));
        assert_eq!(pings("tcp"), (3, 60));
        assert_eq!(pings("http"), (1, 60));
        assert_eq!(pings("nostr"), (1, 60));
        assert_eq!(pings("set"), (3, 30));
    }

    #[test]
    fn aggressive_connection_rates_need_allow_aggressive() {
        let error = error_of("[targets.a]\nurl = \"https://example.com/\"\npings = 20");
        assert!(error.contains("`a`"), "{error}");
        assert!(error.contains("allow_aggressive"), "{error}");
        assert!(
            error_of(
                "[targets.a]\nhost = \"x\"\nprobe = \"tcp\"\nport = 443\npings = 2\nstep = \"15s\""
            )
            .contains("8 connections a minute")
        );
        Config::parse("[defaults]\npings = 20\n[targets.a]\nhost = \"1.1.1.1\"\nprobe = \"dns\"\nname = \"x\"").unwrap();
        Config::parse("[targets.a]\nurl = \"https://example.com/\"\npings = 6").unwrap();
        Config::parse("[defaults]\npings = 20\n[targets.a]\nhost = \"1.1.1.1\"").unwrap();
        let allowed = Config::parse(
            "[targets.a]\nurl = \"https://example.com/\"\npings = 20\nallow_aggressive = true",
        )
        .unwrap();
        assert_eq!(allowed.targets[0].pings, 20);
        assert!(allowed.targets[0].allow_aggressive);
    }

    #[test]
    fn rejects_mistakes_with_the_target_named() {
        assert!(
            error_of("[targets.a]\nhost = \"x\"\nalerts = [\"nope\"]")
                .contains("unknown alert `nope`")
        );
        assert!(error_of("[targets.a]\nhost = \"x\"\nstep = \"1s\"").contains("at least 5s"));
        assert!(error_of("[targets.a]\nhost = \"x\"\npings = 50").contains("1 to 20"));
        assert!(error_of("[targets.a]\nhost = \"x\"\nprobe = \"tcp\"").contains("needs a port"));
        assert!(
            error_of("[targets.a]\nurl = \"https://x/\"\nmethod = \"POST\"")
                .contains("not HEAD or GET")
        );
        assert!(
            error_of("[targets.a]\nhost = \"x\"\ndepends = [\"b\"]").contains("unknown target `b`")
        );
        assert!(error_of("[targets.a]\nhost = \"x\"\nwobble = 1").contains("wobble"));
        assert!(error_of("[targets.a]\ntags = [\"x\"]").contains("no host or url"));
        assert!(error_of("[alerts.x]\nwhen = \"median 20\"").contains("alert `x`"));
        assert!(
            error_of("[targets.a]\nhost = \"x\"\n[targets.a.b]\nhost = \"y\"")
                .contains("cannot also hold")
        );
    }
}
