mod notify;
mod run;

use std::net::SocketAddr;

const DEFAULT_LISTEN: &str = "127.0.0.1:8480";

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use haze_core::config::MIN_STEP;
use haze_core::duration::{format_ms, parse_duration};
use haze_core::{Config, ProbeKind, Target, smoke_line};
use haze_probe::{Sampler, build};

#[derive(Parser)]
#[command(
    name = "haze",
    version,
    about = "Latency and loss monitoring with smoke graphs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Ping a host once and print its smoke line")]
    Ping {
        host: String,
        #[arg(short = 'c', long, default_value_t = 20)]
        count: u32,
        #[arg(short = 'i', long, default_value = "1s", value_parser = parse_duration)]
        interval: Duration,
        #[arg(short = 'W', long, default_value = "2s", value_parser = parse_duration)]
        timeout: Duration,
        #[arg(long, value_parser = ["icmp", "tcp", "udp", "http", "nostr", "dns", "smtp"], default_value = "icmp")]
        probe: String,
        #[arg(long, help = "Name to look up with the dns probe")]
        name: Option<String>,
        #[arg(long, help = "Record type for the dns probe", default_value = "A")]
        record: String,
        #[arg(long, help = "Event id the nostr probe asks for")]
        event: Option<String>,
        #[arg(short = 'p', long)]
        port: Option<u16>,
        #[arg(short = '4', conflicts_with = "ipv6")]
        ipv4: bool,
        #[arg(short = '6')]
        ipv6: bool,
        #[arg(short = 's', long, default_value_t = 56)]
        size: usize,
        #[arg(short = 'I', long)]
        source: Option<String>,
        #[arg(short = 'Q', long)]
        tos: Option<u8>,
    },
    #[command(about = "Check a config file and list its targets")]
    Check {
        #[arg(env = "HAZE_CONFIG", default_value = "haze.toml")]
        config: PathBuf,
    },
    #[command(about = "Probe every target in a config, store the steps and evaluate alerts")]
    Run {
        #[arg(env = "HAZE_CONFIG", default_value = "haze.toml")]
        config: PathBuf,
        #[arg(long, env = "HAZE_DATA")]
        data: Option<PathBuf>,
        #[arg(long = "for", value_parser = parse_duration)]
        duration: Option<Duration>,
        #[arg(short, long)]
        quiet: bool,
        #[arg(long, env = "HAZE_LISTEN", conflicts_with = "no_web")]
        listen: Option<SocketAddr>,
        #[arg(long)]
        no_web: bool,
    },
    #[command(about = "Summarise stored steps per target")]
    Show {
        #[arg(env = "HAZE_DATA", default_value = "data")]
        dir: PathBuf,
        #[arg(long, default_value = "1h", value_parser = parse_duration)]
        last: Duration,
        target: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    match cli.command {
        Command::Ping {
            host,
            count,
            interval,
            timeout,
            probe,
            name,
            record,
            event,
            port,
            ipv4,
            ipv6,
            size,
            source,
            tos,
        } => {
            if count == 0 || count > haze_core::config::MAX_PINGS {
                bail!("count must be 1 to {}", haze_core::config::MAX_PINGS);
            }
            let kind = ProbeKind::parse(&probe).map_err(anyhow::Error::msg)?;
            let by_url = matches!(kind, ProbeKind::Http | ProbeKind::Nostr);
            if kind == ProbeKind::Dns && name.is_none() {
                bail!("the dns probe needs --name");
            }
            let target = Target {
                path: host.clone(),
                title: None,
                host: (!by_url).then(|| host.clone()),
                url: by_url.then(|| host.clone()),
                name,
                record: Some(record.to_ascii_uppercase()),
                event,
                method: None,
                allow_aggressive: true,
                hide_address: false,
                probe: kind,
                step: (interval * count).max(MIN_STEP),
                pings: count,
                timeout,
                family: if ipv4 {
                    Some(4)
                } else if ipv6 {
                    Some(6)
                } else {
                    None
                },
                source,
                size,
                tos,
                port,
                tags: Vec::new(),
                alerts: Vec::new(),
                depends: Vec::new(),
            };
            if matches!(kind, ProbeKind::Tcp | ProbeKind::Udp) && port.is_none() {
                bail!("the {probe} probe needs --port");
            }
            runtime.block_on(ping(target, interval * count))
        }
        Command::Check { config } => {
            let config = load(&config)?;
            print!("{}", describe(&config));
            Ok(())
        }
        Command::Run {
            config,
            data,
            duration,
            quiet,
            listen,
            no_web,
        } => {
            let mut config = load(&config)?;
            for warning in config.warnings() {
                eprintln!("warning: {warning}");
            }
            if let Some(data) = data {
                config.storage_dir = data.display().to_string();
            }
            let listen = match (no_web, listen, &config.listen) {
                (true, _, _) => None,
                (false, Some(listen), _) => Some(listen),
                (false, None, Some(text)) => Some(
                    text.parse()
                        .with_context(|| format!("[web] listen `{text}` is not an address:port"))?,
                ),
                (false, None, None) => Some(DEFAULT_LISTEN.parse()?),
            };
            let options = run::Options {
                duration,
                quiet,
                listen,
                notify: config.notify.clone(),
            };
            runtime.block_on(run::run(config, options))
        }
        Command::Show { dir, last, target } => show(&dir, last, target.as_deref()),
    }
}

pub fn load(path: &PathBuf) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {} (pass a config path or set HAZE_CONFIG)",
            path.display()
        )
    })?;
    Config::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn describe(config: &Config) -> String {
    let mut out = format!(
        "{} targets, {} alerts, data in {} for {} days\n",
        config.targets.len(),
        config.alerts.len(),
        config.storage_dir,
        config.retention_days,
    );
    for t in &config.targets {
        let what = t.url.as_deref().or(t.host.as_deref()).unwrap_or_default();
        let port = t.port.map(|p| format!(":{p}")).unwrap_or_default();
        out.push_str(&format!(
            "  {:<28} {:<5} {}{} every {}s x{} timeout {}ms",
            t.path,
            t.probe.name().to_string(),
            what,
            port,
            t.step.as_secs(),
            t.pings,
            t.timeout.as_millis(),
        ));
        if !t.tags.is_empty() {
            out.push_str(&format!(" tags {}", t.tags.join(",")));
        }
        if !t.alerts.is_empty() {
            out.push_str(&format!(" alerts {}", t.alerts.join(",")));
        }
        out.push('\n');
    }
    for warning in config.warnings() {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out
}

pub fn scale_for(max: Option<Duration>) -> Duration {
    let ms = max.map(|m| m.as_secs_f64() * 1e3).unwrap_or(1.0).max(0.1) * 1.1;
    let magnitude = 10f64.powf(ms.log10().floor());
    let nice = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|f| f * magnitude)
        .find(|v| *v >= ms)
        .unwrap_or(10.0 * magnitude);
    Duration::from_secs_f64(nice / 1e3)
}

async fn ping(target: Target, span: Duration) -> Result<()> {
    let probe = build(&target).await.map_err(anyhow::Error::msg)?;
    let sampler = Sampler::new(Arc::clone(&probe));
    let (step, detail) = sampler.step(target.pings, span, target.timeout).await;
    let summary = step.summary();
    let scale = scale_for(summary.max);
    println!(
        "{} {} pings over {:.1}s, scale 0-{} ms",
        target.path,
        target.pings,
        span.as_secs_f64(),
        format_ms(scale)
    );
    println!("{}", smoke_line(&step, scale));
    if let Some(reply) = &detail.last_reply {
        let phases: Vec<String> = reply
            .phases
            .iter()
            .map(|(n, d)| format!("{n} {}", format_ms(*d)))
            .collect();
        if !phases.is_empty() {
            println!("last: {} ms", phases.join(", "));
        }
        if let Some(note) = &reply.note {
            println!("last: {note}");
        }
    }
    if step.duplicates > 0 || step.reordered > 0 {
        println!(
            "{} duplicates, {} out of order",
            step.duplicates, step.reordered
        );
    }
    for (seq, error) in detail.errors.iter().take(3) {
        println!("seq {seq}: {error}");
    }
    if summary.received == 0 {
        bail!("no replies from {}", target.path);
    }
    Ok(())
}

fn show(dir: &PathBuf, last: Duration, only: Option<&str>) -> Result<()> {
    let store = haze_store::Store::open(dir)?;
    let now = run::now_ms();
    let from = now - last.as_millis() as i64;
    for target in store.targets()? {
        if only.is_some_and(|o| o != target) {
            continue;
        }
        let buckets = store.query(&target, from, now + 1, now + 1 - from)?;
        let Some(b) = buckets.first() else {
            continue;
        };
        let h = &b.histogram;
        let ms = |d: Option<Duration>| d.map(format_ms).unwrap_or_else(|| "-".into());
        println!(
            "{target:<28} {:>4} steps {:>6} pings loss {:>5.1}%{} min {} p10 {} med {} p90 {} max {} ms",
            b.steps,
            b.sent,
            b.loss() * 100.0,
            if b.refused > 0 {
                format!(" refused {}", b.refused)
            } else {
                String::new()
            },
            ms(h.min()),
            ms(h.quantile(0.1)),
            ms(h.quantile(0.5)),
            ms(h.quantile(0.9)),
            ms(h.max()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_prints_warnings_after_the_targets() {
        let config =
            Config::parse("[targets.ssh]\nhost = \"example.com\"\nprobe = \"tcp\"\nport = 22")
                .unwrap();
        let text = describe(&config);
        assert!(text.contains("warning: target `ssh`"), "{text}");
        assert!(text.contains("every 60s x3"), "{text}");
    }

    #[test]
    fn shipped_examples_parse() {
        for text in [
            include_str!("../../../examples/haze.toml"),
            include_str!("../../../examples/live.toml"),
        ] {
            Config::parse(text).unwrap();
        }
    }

    #[test]
    fn scale_rounds_up_to_a_nice_number() {
        assert_eq!(
            scale_for(Some(Duration::from_millis(8))),
            Duration::from_millis(10)
        );
        assert_eq!(
            scale_for(Some(Duration::from_millis(12))),
            Duration::from_millis(20)
        );
        assert_eq!(
            scale_for(Some(Duration::from_millis(150))),
            Duration::from_millis(200)
        );
        assert_eq!(
            scale_for(Some(Duration::from_micros(30))),
            Duration::from_micros(200)
        );
        assert_eq!(scale_for(None), Duration::from_millis(2));
    }
}
