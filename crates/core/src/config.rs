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
pub struct RawConfig {
    #[serde(default)]
    pub defaults: Settings,
    pub storage: Option<Storage>,
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
}

impl ProbeKind {
    fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "icmp" | "ping" => ProbeKind::Icmp,
            "tcp" => ProbeKind::Tcp,
            "udp" => ProbeKind::Udp,
            "http" | "https" => ProbeKind::Http,
            other => return Err(format!("`{other}` is not a probe (icmp, tcp, udp, http)")),
        })
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
    pub tags: Vec<String>,
    pub alerts: Vec<String>,
    pub depends: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub storage_dir: String,
    pub retention_days: u32,
    pub alerts: BTreeMap<String, Alert>,
    pub targets: Vec<Target>,
}

pub const MIN_STEP: Duration = Duration::from_secs(5);
pub const MAX_PINGS: u32 = 20;

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
        for target in &targets {
            for alert in &target.alerts {
                if !alerts.contains_key(alert) {
                    return Err(format!("target `{}` uses unknown alert `{alert}`", target.path));
                }
            }
            for dep in &target.depends {
                if !targets.iter().any(|t| &t.path == dep) {
                    return Err(format!("target `{}` depends on unknown target `{dep}`", target.path));
                }
            }
        }
        let storage = raw.storage.unwrap_or(Storage {
            dir: "data".into(),
            retention_days: None,
        });
        Ok(Config {
            storage_dir: storage.dir,
            retention_days: storage.retention_days.unwrap_or(400),
            alerts,
            targets,
        })
    }

    pub fn target(&self, path: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.path == path)
    }
}

const NODE_KEYS: [&str; 15] = [
    "title", "host", "url", "tags", "alerts", "depends", "probe", "step", "pings", "timeout",
    "family", "source", "size", "tos", "port",
];

fn walk(table: &toml::Table, prefix: &str, parent: &Inherited, out: &mut Vec<Target>) -> Result<(), String> {
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
            return Err(format!("`{path}` has no host or url and no targets under it"));
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
        (None, Some(_)) => ProbeKind::Http,
        (None, None) => ProbeKind::Icmp,
    };
    let duration = |field: &str, value: &Option<String>, default: Duration| -> Result<Duration, String> {
        value
            .as_deref()
            .map(parse_duration)
            .transpose()
            .map(|d| d.unwrap_or(default))
            .map_err(|e| format!("target `{path}` {field}: {e}"))
    };
    let step = duration("step", &s.step, Duration::from_secs(60))?;
    let timeout = duration("timeout", &s.timeout, Duration::from_secs(2))?;
    let pings = s.pings.unwrap_or(MAX_PINGS);
    if step < MIN_STEP {
        return Err(format!("target `{path}`: step must be at least {}s", MIN_STEP.as_secs()));
    }
    if pings == 0 || pings > MAX_PINGS {
        return Err(format!("target `{path}`: pings must be 1 to {MAX_PINGS}"));
    }
    if timeout >= step {
        return Err(format!("target `{path}`: timeout must be shorter than the step"));
    }
    if let Some(f) = s.family
        && f != 4
        && f != 6
    {
        return Err(format!("target `{path}`: family is 4 or 6, not {f}"));
    }
    if probe == ProbeKind::Http && node.url.is_none() {
        return Err(format!("target `{path}`: the http probe needs a url"));
    }
    if probe != ProbeKind::Http && node.host.is_none() {
        return Err(format!("target `{path}`: the {probe:?} probe needs a host"));
    }
    if matches!(probe, ProbeKind::Tcp | ProbeKind::Udp) && s.port.is_none() {
        return Err(format!("target `{path}`: the {probe:?} probe needs a port"));
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
pings = 20
timeout = "2s"

[storage]
dir = "/var/lib/pingwatch"

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
            ["dns/cloudflare", "dns/google", "dns/v6/cloudflare", "edge/router", "web/example", "web/ssh"]
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

        assert_eq!(config.storage_dir, "/var/lib/pingwatch");
        assert_eq!(config.retention_days, 400);
        assert_eq!(config.alerts["loss"].policy.clear_after, 2);
        assert!(matches!(
            config.alerts["slow"].matcher,
            Matcher::Threshold { baseline_steps: 60, .. }
        ));
    }

    fn error_of(text: &str) -> String {
        Config::parse(text).expect_err("should not parse")
    }

    #[test]
    fn rejects_mistakes_with_the_target_named() {
        assert!(error_of("[targets.a]\nhost = \"x\"\nalerts = [\"nope\"]").contains("unknown alert `nope`"));
        assert!(error_of("[targets.a]\nhost = \"x\"\nstep = \"1s\"").contains("at least 5s"));
        assert!(error_of("[targets.a]\nhost = \"x\"\npings = 50").contains("1 to 20"));
        assert!(error_of("[targets.a]\nhost = \"x\"\nprobe = \"tcp\"").contains("needs a port"));
        assert!(error_of("[targets.a]\nhost = \"x\"\ndepends = [\"b\"]").contains("unknown target `b`"));
        assert!(error_of("[targets.a]\nhost = \"x\"\nwobble = 1").contains("wobble"));
        assert!(error_of("[targets.a]\ntags = [\"x\"]").contains("no host or url"));
        assert!(error_of("[alerts.x]\nwhen = \"median 20\"").contains("alert `x`"));
        assert!(error_of("[targets.a]\nhost = \"x\"\n[targets.a.b]\nhost = \"y\"").contains("cannot also hold"));
    }
}
