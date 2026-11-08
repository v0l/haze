# Design

Each section gives the options, the choice and why. "Skeleton" marks what this first cut builds;
the rest is the plan.

## Layout

A Cargo workspace like cycler: small crates and one binary that is both the CLI and the web
dashboard. There is no desktop app; the dashboard is the only UI.

| Crate | Holds |
| --- | --- |
| `haze-core` | Probe trait, samples and step summaries, the log histogram, matchers and alert state, config types and inheritance. No async runtime, no I/O. |
| `haze-probe` | ICMP echo, UDP echo, TCP connect, HTTP/HTTPS timing on tokio. |
| `haze-store` | The day file store and its query API. |
| `haze` | The binary: `ping`, `check`, `run` (which also serves the dashboard), `show`. Later `agent`. |
| `haze-web` | axum routes, the JSON API, the SVG graph renderer and the embedded client. |

## 1. Probes

Options: fork external tools like SmokePing (fping, curl), use one crate per protocol
(`surge-ping`, `reqwest`), or write the probes on `socket2` and tokio.

Choice: our own probes on `socket2` and tokio, behind one trait in `haze-core`:

```rust
pub trait Probe: Send + Sync {
    fn probe(&self, seq: u16, timeout: Duration) -> ProbeFuture<'_>;
    fn duplicates(&self) -> u64 { 0 }
}
```

One call is one ping. The scheduler decides when to call it, so every probe gets spread sampling,
timeouts and loss accounting for free, and a third party probe is one small type. A reply carries
the RTT and named phases (`dns`, `connect`, `tls`, `ttfb`) for probes that have them.

Why not the crates: `reqwest` hides connect, TLS and first byte timings; `surge-ping` owns the
receive path and timestamps. The ICMP code needed is about 200 lines.

- **ICMP** (skeleton): `SOCK_DGRAM` with `IPPROTO_ICMP` or `IPPROTO_ICMPV6`, so no root and no
  capability on Linux (`net.ipv4.ping_group_range`, which is `0 2147483647` here) and on macOS.
  Each target has its own socket and a receive task that matches replies to waiting pings by
  sequence number and a random 8 byte token in the payload; macOS hands every reply to every
  socket and includes the IPv4 header, so both are handled. On Linux the receive time is the
  kernel's `SO_TIMESTAMPNS` stamp, which leaves scheduler wake up latency out of the RTT. Raw
  sockets (with `CAP_NET_RAW`) and `IcmpSendEcho2` on Windows come later.
- **UDP echo** (skeleton): a datagram with a token to an echo port, timed to its return. Used for
  RFC 862 echo servers and for the accuracy test.
- **TCP connect** (skeleton): time to an established connection, the TCPPing equivalent.
- **HTTP/HTTPS** (skeleton): our own HTTP/1.1 client on tokio and rustls, timing DNS, connect,
  TLS, time to first byte and total, with status and the certificate's days to expiry.
- **DNS** (next): `hickory-resolver` or a hand built query over UDP, timed per server.
- **Path** (next): mtr style per hop loss and RTT with TTL limited DGRAM probes; `trippy-core`
  is the fallback if hand rolling gets long.
- **Nostr relay** (next): WebSocket connect time, then a REQ with a fresh subscription id timed to
  EOSE, and optionally an EVENT timed to its OK, for the user's relays.
- **TWAMP-light, STUN, QUIC handshake, looking glass** (later): TWAMP-light reflector is a UDP
  probe with timestamps in the payload; STUN binding request is a UDP probe; QUIC via `quinn`;
  the lg.lnvps.cloud check as an HTTP probe with a body matcher.
- Every probe takes the address family (4, 6 or either), source address, packet size and
  TOS/DSCP. Source interface binding (`SO_BINDTODEVICE`) is Linux only.
- External process probes (a program that prints one RTT per line) come before WASM; WASM only if
  people ask.

## 2. Sampling

Options: a burst of N pings per step (SmokePing, fping `-C`), continuous pings exported as a
histogram (smokeping_prober), or N pings spread over the step.

Choice: N pings spread evenly over the step (default 20 per 60 s), each with its own timeout, so
the step describes the whole interval at the same packet cost. Every RTT is kept. Per step we
record pings sent, the RTT of each sequence number or a loss, duplicates and reordering
(a reply that arrives after a reply to a later sequence number). The summary is derived, never
stored: loss, min, p10, p25, median, p75, p90, max, mean, RFC 3550 jitter over consecutive
replies, and PDV as p99 minus min (RFC 5481).

The smoke distribution is a log bucket histogram (a DDSketch style layout): bucket `i` holds
values in `[1us * g^i, 1us * g^(i+1))` with `g = 1.02`, and a quantile is read as the bucket's
harmonic midpoint, so every quantile is within 1% of the exact value. Counts are sparse, so a
step of 20 RTTs is at most 20 entries. Merging is adding counts, which is what makes long ranges
honest. HdrHistogram was the other candidate; its layout spends far more buckets for the same
relative error across 1 us to 100 s, and it brings little else here.

## 3. Storage

Options: RRD files (fixed resolution, averaged percentiles), Prometheus or VictoriaMetrics only
(needs a second service, and histograms with fixed buckets), an embedded KV store (redb, fjall),
DuckDB (heavy C++ build, bad on musl and cross targets), or a purpose built file per target per
day.

Choice: a file per target per UTC day, `data/<target path>/<YYYY-MM-DD>.haze`, appended one record
per step. A record is a length, a CRC32 and the step: start time, step length, pings sent,
duplicates, reordered, and one `u32` microseconds RTT per ping with `u32::MAX` for a loss. 20
pings every 60 s is 106 bytes a step and about 150 KB a day per target (measured in the demo),
60 MB per target for 400 days. A torn last
record (power cut) fails its CRC and is cut off when the file is next opened for writing, so the
store is crash safe without a WAL. Retention is deleting old day files. Copying, backing up and
inspecting are file operations.

Queries (skeleton) read the days in range and merge steps into buckets of the requested width,
each bucket a merged histogram with sent and received counts. Rollup tiers come next: hourly and
daily files of serialised histograms written when a day closes, so a year view reads 365 small
records instead of 525,600 steps. Rollups are histograms, so their percentiles are real.

Export (next): `/metrics` in OpenMetrics with native histogram style buckets and the summary
quantiles as gauges, Influx line protocol for VictoriaMetrics and InfluxDB, and a reader for a
Prometheus `smokeping_prober` histogram so its data can be graphed here. Import of SmokePing RRD
data through `rrdtool dump` XML: the pingN columns become one step record each, at whatever
resolution the RRA still has.

Alert history, users and API tokens go in one redb file beside the data; samples do not.

## 4. UI

Options: a Preact client served by axum, an egui app compiled to WASM, or server rendered pages.

Choice: one binary serves everything with axum. Pages are server rendered HTML, and the graphs
are SVG drawn in Rust from the merged histograms: the smoke band as stacked quantile areas,
the median line coloured by loss, loss as a strip underneath. The same renderer produces the
PNG and SVG exports and the share link images, so a graph looks the same everywhere and works
without JavaScript. A small Preact client adds zoom, pan, crosshair and live updates by
fetching `/api/series` JSON and drawing on a canvas with one time axis shared by every graph on
the page. egui in WASM was rejected: a large download, weak text and accessibility, and poor on
phones.

Navigation is tags as well as the tree: a target path (`dns/cloudflare`) and any number of tags
(`dns`, `anycast`, `site:dub`). Views: overview grid, single target with every vantage point
overlaid, compare (any targets overlaid), loss heatmap (targets by time), incident timeline. A
share link fixes the range in the URL. Dark mode from `prefers-color-scheme`, a mobile layout.

## 5. Alerts

Choice: a small expression language in config, evaluated after every step per target.

```
loss >= 10% for 3 steps
median > 20ms 3 of 5
p90 > 2x baseline 4 of 6
smoke > 3x baseline for 2 steps
loss pattern ==0%,==0%,>20%,>20%
rtt pattern <10,<10,>100,>100
```

A metric (`loss`, `min`, `median`, `mean`, `max`, `p10`..`p99`, `jitter`, `smoke` which is p90
minus p10), a comparison, a threshold in ms, % or a multiple of the baseline (the median of the
metric over the previous 288 steps by default, current window excluded), and a window: a step,
`for N steps` or `K of N`. SmokePing patterns are accepted as written (`*`, `U`, `*X*`) so
existing rules move over; `==S` is not supported.

The alert state raises when the expression matches and clears after it has not matched for
`clear_after` steps (default 1). A flap guard counts state changes in the last `flap_window` steps;
past `flap_limit` the alert is marked flapping and notifications stop until it settles.
Dependencies: a target's `depends` names another target; while that one has an active alert the
child's alerts are held. Maintenance windows silence by tag or path for a time range.

Routes (next): Nostr DM (NIP-17) from a haze key, webhook (JSON), SMTP, ntfy, Telegram.
Each route has a filter by tag and severity. History of raises and clears goes in the redb file.

## 6. Distributed

Options: SmokePing style HTTP with a shared secret, mTLS, Noise, or signed requests over HTTPS.

Choice: `haze agent` is the same binary in agent mode. It has a secp256k1 key (a Nostr key,
so the user's tooling and NIP-98 style HTTP auth apply) and the server allowlists agent public
keys. The agent fetches its assignment (the targets tagged for it) and pushes batches of step
records as signed HTTPS requests. It writes every step to its own local store first and keeps a
cursor of the last record the server acknowledged, so being offline is just a gap in the
acknowledgements and back filling is sending from the cursor. The server stores agent data as
`data/<target path>@<agent>/`. The binary is static musl, about 10 MB, for x86_64 and aarch64
(Pi), with a scratch container image.

mTLS was rejected for the CA it makes the user run; Noise for a second protocol stack with no
gain over HTTPS plus signatures.

## 7. Config

Options: TOML, KDL, YAML, SmokePing's own format.

Choice: one TOML file. Targets are a tree of tables under `[targets]`; a table with `host` or
`url` is a target, any other table is a group whose settings its children inherit. Tags and alert
lists add up down the tree, everything else is overridden.

```toml
[defaults]
step = "60s"
pings = 20
timeout = "2s"

[alerts.loss]
when = "loss >= 10% for 3 steps"

[targets.dns]
tags = ["dns"]
alerts = ["loss"]

[targets.dns.cloudflare]
host = "1.1.1.1"

[targets.web.example]
probe = "http"
url = "https://example.com/"
```

`haze check config.toml` resolves the tree, parses every alert and prints the targets
(skeleton). Live reload on file change and SIGHUP, keeping each unchanged target's schedule and
state. An HTTP API adds and removes runtime targets, kept in redb and shown apart from file ones.
`haze import smokeping Targets` converts a SmokePing Targets file (its `+`, `++` tree maps to
nested tables; `smokeping-config` only writes Targets files, so this is our own small parser),
and `haze import hosts` makes one target per line of a hosts list.

## 8. Packaging

Choice: follow agentbench's release workflow. A `v*` tag builds release archives for
x86_64 and aarch64 Linux (musl, static), aarch64 macOS and x86_64 Windows, plus `.deb` and `.rpm`
(`cargo-deb`, `cargo-generate-rpm`), a scratch Docker image pushed on tag, a systemd unit with
`DynamicUser=yes` and `StateDirectory=haze`, and checksums with signed artifacts. The
CHANGELOG follows Keep a Changelog as in cycler. Nothing is published until the user picks the
name and the GitHub org.
