# Research

What SmokePing does, what the tools that came after it do better or worse, which Rust crates
exist for the parts, and what is worth copying.

## SmokePing's model

SmokePing (Tobi Oetiker, Perl, RRDtool, a CGI front end) measures latency and loss to a tree of
targets.

- **Step and pings.** The `Database` section sets `step` (default 300 s) and `pings` (default 20,
  minimum 3). Each step a probe sends `pings` probes to every target and gets back a list of RTTs.
  FPing sends them as a burst (fping `-C 20`), so the 20 values describe a few seconds, not the
  whole 5 minutes.
- **Smoke.** The RTTs are sorted and stored as `pings` separate RRD data sources (`ping1` to
  `ping20`) plus `median` and `loss`. The graph draws the median as a line coloured by loss and
  shades the band between the sorted values in grey, darker where values cluster. That shaded
  band is the "smoke" and it is the whole point: the shape of the distribution, not an average.
- **RRAs.** The default RRAs are `AVERAGE 0.5 1 1008` (one week of 5 minute steps), then
  `AVERAGE/MIN/MAX 0.5 12 4320` (one hour points for 180 days) and `AVERAGE/MIN/MAX 0.5 144 720`
  (12 hour points for 360 days). Consolidating `ping7` with AVERAGE averages the 7th smallest value
  across steps, which is not a percentile of anything. Old graphs look smooth because the
  distribution has been averaged away.
- **Config.** One Perl-ish file with `*** Section ***` headers and `+`, `++`, `+++` for tree
  depth. `General`, `Database`, `Presentation`, `Probes`, `Slaves`, `Targets`, `Alerts`. Probe
  sections can have subsections (`+FPingLarge` with `packetsize = 5000`) that inherit from the
  probe; targets inherit probe, alerts, slaves and most settings from their parent node.
- **Probes** (shipped): FPing, FPing6, FPingContinuous, DNS and AnotherDNS, Curl and AnotherCurl,
  the EchoPing family (Http, Https, Smtp, DNS, LDAP, Whois, Chargen, Discard, Icp), TCPPing,
  SSH and AnotherSSH, Radius, TacacsPlus, LDAP, SipSak, NFSping, IRTT, TraceroutePing, the router
  probes (CiscoRTTMon*, IOSPing, TelnetIOSPing, TelnetJunOSPing, OpenSSHJunOSPing,
  OpenSSHEOSPing, DismanPing), SendEmail, FTPtransfer, WebProxyFilter, Qstat. Most of them fork
  an external tool (fping, curl, echoping, tcpping) and parse its text output. A probe is a Perl
  module on top of `basefork` or `basevars`.
- **Alerts.** A pattern is a comma separated list of comparisons, oldest first, matched against
  the most recent steps: `==0%,==0%,==0%,>20%,>20%,>20%` for loss, `<10,<10,>100,>100` for RTT
  in ms. `*` matches any value, `U` unknown data (with `==`), `==S` the start of SmokePing, and
  `*X*` skips up to X values, so `>10%,*10*,>10%` is "more than 10% loss twice in 12 steps".
  `type = matcher` calls a plugin: `Median`, `Medratio` and `Avgratio` (current median or
  average over N steps against a historic one over M steps, compared with a percentage),
  `CheckLatency` and `CheckLoss`. `edgetrigger = yes` notifies on raise and clear only. Alerts go
  to email, SNPP or a program called with the alert name, target and the loss and RTT history.
- **Master and slaves.** A slave has no config of its own. It asks the master over the normal
  CGI URL for its assignments, runs its probes, then posts the results; the master tells it about
  config changes in the reply. Messages are signed with an MD5 HMAC-ish hash using a shared
  secret per slave. Results that cannot be delivered are kept in a Perl Storable file and sent
  with the next round. The master stores them as `Target~Slave.rrd`.

### What is dated

- Averaging percentiles in the RRAs destroys the smoke at longer ranges, and an RRD file has a
  fixed size and fixed resolution chosen up front.
- The UI is PNGs rendered by the CGI per request. No zoom beyond "pick a range", no overlay of
  targets, no live view, no API, no mobile layout.
- Bursts of 20 pings measure a few seconds out of every 300.
- ICMP needs fping with setuid or `cap_net_raw`; IPv6 is a separate probe (FPing6) and target.
- Probes fork tools and parse text. Adding one means Perl.
- Alerts are email or a script, notified on every match unless edge triggered, with no history,
  no dependencies and no maintenance windows.
- MD5 signed slave traffic and a Storable file buffer.

## Modern alternatives

| Tool | What it does better | What it does worse |
| --- | --- | --- |
| `smokeping_prober` (SuperQ, Go) | The closest idea: pings continuously (default 1 s), exports `smokeping_response_duration_seconds` as a Prometheus histogram with exponential buckets (50 us to 26 s, doubling), plus duplicates, TTL and send/receive errors. YAML config reloaded on SIGHUP or `POST /-/reload`. ICMP or UDP. | No UI, no storage, no alerts of its own: needs Prometheus and Grafana. Doubling buckets are coarse (a 10 ms RTT lands in 6.4 to 12.8 ms). ICMP needs root or `cap_net_raw`. Only ping. |
| Prometheus `blackbox_exporter` + Grafana | Many probe modules (ICMP, TCP, HTTP with phase timings and certificate expiry, DNS, gRPC). PromQL alerting, Alertmanager routing, mature. | One probe per scrape, so no distribution per step unless you scrape every second. Three moving parts to run. The smoke has to be rebuilt with heatmap panels. |
| VictoriaMetrics | Cheap long retention, `vmrange` histogram buckets that merge correctly, Prometheus compatible. | Only storage. |
| Gatus (Go) | Endpoint checks with conditions in YAML (`[STATUS] == 200`, `[RESPONSE_TIME] < 300`), many alert providers, status page. | Up/down thinking: one result per interval, no distribution, no loss model. |
| Uptime Kuma (Node) | Friendly UI first setup, SQLite, many notification targets including Telegram and ntfy, status pages. | Configuration is in the UI, not a file. One sample per interval, latency as a line. Heavy. |
| Netdata | Per second collection, an fping plugin, zero setup dashboards. | Large agent, cloud oriented, the ping view is a line chart, retention tiers average. |
| Grafana Synthetic Monitoring | Hosted probes in many regions, k6 scripted checks, ping, DNS, HTTP, traceroute. | SaaS, cost per check, no smoke. |
| OWAMP / TWAMP (perfSONAR) | One way delay and loss, standard protocol (RFC 4656, RFC 5357), TWAMP-light for reflector tests. | Needs synchronised clocks for one way numbers, heavy perfSONAR stack. |
| RIPE Atlas | Thousands of vantage points, public measurements, ping/traceroute/DNS/HTTP/TLS, API. | Credits, not continuous for your own targets at 20 pings per step, not self hosted. |
| Cloudflare Radar | Internet wide view of outages and routing. | Not your targets. Useful as context links in an incident. |
| NetBeez | Agents at branch offices, path analysis, a polished multi-site UX. | Commercial. |
| PingPlotter | Traceroute plus a timeline per hop in one view, very good for showing a provider where loss starts. | Desktop, commercial, one machine. |
| trippy (Rust) | An mtr style TUI with per hop loss and jitter, works unprivileged on Linux via DGRAM sockets. | Interactive only, no history. Its `trippy-packet` and `trippy-core` crates are useful for a path probe. |

### Ideas to copy

- The smoke graph and the sorted RTT band (SmokePing), coloured by loss.
- A histogram per step that merges correctly, so a year view is still a real distribution
  (smokeping_prober, VictoriaMetrics `vmrange`).
- Continuous, spread out sampling rather than a burst (smokeping_prober, Netdata).
- HTTP phase timings and certificate expiry (blackbox_exporter).
- Readable conditions in config (Gatus), and SmokePing patterns for people moving over.
- Notification targets that people use today: ntfy, Telegram, webhook, SMTP (Uptime Kuma), and
  Nostr DMs for this user's stack.
- Reload on SIGHUP and on an HTTP call (smokeping_prober).
- Per hop timeline for incidents (PingPlotter, trippy).
- Agents that buffer and back fill (SmokePing slaves), with modern signing.
- Prometheus and Influx export, so it fits an existing Grafana rather than replacing it.

## Rust crates

Versions and all time downloads from crates.io on the day of writing.

| Need | Crate | Notes |
| --- | --- | --- |
| ICMP | `socket2` 0.6.5 (1.4 G) | `Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4))` is all an unprivileged ping needs; set non-blocking and wrap in `tokio::io::unix::AsyncFd`. |
| ICMP | `surge-ping` 0.9.1 (4.5 M) | Async pinger on socket2, supports DGRAM and RAW. Its client owns the receive loop and the identifier logic; fine, but we want our own timestamps and payload token, and the code we need is about 200 lines. |
| ICMP | `tokio-icmp-echo` 0.4.3, `ping` 0.10, `fastping-rs`, `oping` (liboping) | Smaller or older. `winping` 0.10 wraps `IcmpSendEcho` for unprivileged ping on Windows. |
| Packets | `pnet` 0.35 (21 M), `trippy-packet` 0.13 | For raw sockets, traceroute and TTL games. Not needed for echo. |
| DNS | `hickory-resolver` 0.26.3 (98 M) | Stub resolver for the DNS probe and timed lookups. |
| Histograms | `hdrhistogram` 7.6 (129 M) | Fixed relative precision, merges, serialises (V2 format). Linear-of-logs layout wastes buckets for RTTs; the serialised form is larger than a sparse log sketch. |
| Histograms | `sketches-ddsketch` 0.4.2 (106 M) | DDSketch: log buckets with a relative error bound, merges. No stable compact on-disk format of its own. |
| Quantiles | `quantiles` 0.7.1 | CKMS and Greenwald-Khanna. Mergeable only in a weak sense. |
| Web | `axum` 0.8.9 (576 M) | Server and API. |
| SQL | `sqlx` 0.9 | For alert history and users if they grow; not for samples. |
| KV stores | `redb` 4.5 (16 M), `fjall` 3.1 (2 M), `sled` 0.34 (unmaintained beta) | redb is a copy-on-write B-tree with ACID commits, a good fit for metadata. fjall is an LSM, good for heavy writes. |
| Columnar | `duckdb` 1.2 | Very good for ad hoc analytics, but a C++ build that is slow and hard on musl and cross targets. |
| HTTP probe | `tokio-rustls` with `rustls` (ring) and `webpki-roots`; `x509-parser` | Separate timings for connect, TLS and first byte need our own client rather than `reqwest`. |
| WebSocket | `tokio-tungstenite` 0.30 | Nostr relay probe. |
| Config | `toml` 1.2, `kdl` 6.7 | Both fine. |
| Reload | `notify` 8.2 | File watching for live reload. |

`smokeping-config` 0.1.2 renders SmokePing `Targets` files from a YAML patch on top of a JSON
catalogue. It writes Targets files and does not parse them, so an importer has to parse the
SmokePing format itself (it is a simple indented tree, a small hand written parser).

Unprivileged ICMP on Linux: `net.ipv4.ping_group_range` lists the groups allowed to open
`SOCK_DGRAM` ICMP sockets. On this machine it is `0 2147483647`, so every user may. The kernel
replaces the identifier with the socket's local port, computes the checksum, delivers only the
replies for that identifier and strips the IP header. macOS has the same socket type, keeps the
identifier we set, delivers every echo reply to every such socket and includes the IPv4 header
on receive, so the receiver must skip the header and filter by identifier. Windows has no such
socket; `IcmpSendEcho2` (as `winping` uses) works without admin rights.

## Name

| Name | crates.io | GitHub |
| --- | --- | --- |
| `pingwatch` | free | used by small repos (`fazalmajid/pingwatch`, 36 stars) |
| `haze` | free | many repos with "haze" in the name, `v0l/haze` is free |
| `smoke` | taken (test framework) | crowded |
| `plume` | taken (editor spawner) | crowded |
| `smokewatch`, `fume`, `wisps`, `smokeline` | free | |

`haze` is short, free on crates.io and under `v0l`, and reads as what the graph shows. The user
picked it: crates `haze-core`, `haze-probe`, `haze-store`, `haze-web` and the binary `haze`.
