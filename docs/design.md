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
- **TCP connect** (skeleton): time to an established connection, the TCPPing equivalent. The
  connection is shut down with a FIN and drained, never dropped with a reset.
- **SMTP** (built): connect, read the `220` banner (multi line too), send `QUIT`, time connect
  to banner. A `421` or `554` banner is a refusal.
- **HTTP/HTTPS** (skeleton): our own HTTP/1.1 client on tokio and rustls, timing DNS, connect,
  TLS, time to first byte and total, with status and the certificate's days to expiry. `HEAD`
  by default, falling back to `GET` for good after a 405 or 501.
- **DNS** (built): one query over UDP built with `hickory-proto` (the wire format only, not the
  resolver, so the time is one round trip to the server named, with no cache or retries in the
  way). A fresh socket and random id per query; replies must match the id and the question.
  NXDOMAIN is an answer, SERVFAIL and REFUSED are failures.
- **Path** (next): mtr style per hop loss and RTT with TTL limited DGRAM probes; `trippy-core`
  is the fallback if hand rolling gets long.
- **Refusals** (built): a server that answers 429 or 503 is recorded as `refused` with the status,
  kept out of loss, and makes the target back off.
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

Choice: N pings spread evenly over the step, each with its own timeout, so the step describes
the whole interval at the same packet cost. N defaults by probe: 20 per 60 s for icmp and udp,
5 for dns, 3 for tcp and 1 for http and smtp, because those pings are whole connections
or queries. Connection pings go at a random point inside their slot (`gap * i + rand(0, gap)`),
so they never line up with the clock or with each other. Every RTT is kept. Per step we
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

Politeness. A server sees haze as one client, however many targets point at it, so the limits
are per destination (host and port, with the scheme's default port filled in):

- Config time: the summed rate of connection targets on one destination may not pass 6 a
  minute unless a target there sets `allow_aggressive = true`. Rejecting rather than warning,
  because a ban is slow to undo and the error says exactly what to change.
- Run time: one `Limiter` per destination hands out start slots at least 60 s / budget apart,
  shared by every target on it, so overlapping targets queue instead of adding up.
- Backoff per target: a refusal (HTTP 429 or 503, a refused WebSocket upgrade, TCP reset, TLS
  alert, SMTP 421 or 554) doubles the step up to an hour and takes `Retry-After` when it is
  longer; three clean steps restore it. Refusals by HTTP status count as `refused`, kept out of
  loss. The hub keeps the reason and the time to the next step for the dashboard.

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

Queries read the days in range and merge steps into buckets of the requested width, each bucket
a merged histogram with sent and received counts. When a day has closed, `haze run` writes a
rollup beside it, `<YYYY-MM-DD>.hzr`: one histogram for the day and one per hour, with a CRC and
written through a rename so it is all or nothing. A query whose width and start are whole hours
or days reads the rollups for the parts it covers whole and raw steps for the edges; any other
width reads raw steps. A late write into a rolled day deletes its rollup and a corrupt one is
ignored, so the raw files stay the truth. A 400 day view reads 400 small rollups instead of
576,000 steps. Rollups are histograms, so their percentiles stay within the 1% bucket bound:
a test checks a 7 day query over rolled up data against exact percentiles of every RTT.

Export: `/metrics` in OpenMetrics (built) with the RTT as a histogram on smokeping_prober's
buckets, so existing Grafana boards for it work, plus the last step's loss and quantiles as
gauges. Next: Influx line protocol for VictoriaMetrics and InfluxDB, a reader for a Prometheus
`smokeping_prober` histogram so its data can be graphed here, and import of SmokePing RRD data
through `rrdtool dump` XML: the pingN columns become one step record each, at whatever
resolution the RRA still has.

Alert history is an append-only `alerts.jsonl` beside the data. Incidents still open when haze
stops are closed with a `restart` record on the next start, because the alert state is not kept
across restarts. Users and API tokens, when they come, go in one redb file; samples do not.

## 4. UI

The web dashboard is the only UI (user decision): no desktop app.

Options for the client: Preact built with Vite, an egui app compiled to WASM, or a hand-written
ES module with no build step.

Choice: one binary serves everything with axum, and the graphs are SVG drawn in Rust from the
merged histograms: min to max, p10 to p90 and p25 to p75 as stacked shaded bands, the median as a
line coloured by loss with SmokePing's colours, and steps with every ping lost shaded red. The
same renderer makes the `/api/graph/<target>.svg` endpoint, the PNG export (through `resvg` with
an embedded DejaVu Sans Mono, so it needs no system fonts) and the dashboard's own graphs, so a
graph looks the same everywhere and can be embedded anywhere.

The client is the user's standard stack: Bun, Vite, Preact and TypeScript (strict), in
`crates/web/ui`, laid out as a standard Vite app. `vite-plugin-singlefile` inlines the
script, styles and icon into one `dist/index.html` that the server embeds with `include_str!`,
so there are no asset routes and the API and UI cannot drift apart. `src/types.ts` mirrors the
Rust JSON. A first cut was a hand-written module with no build step; the user chose the standard
stack instead.

Keeping `dist` out of git and failing `build.rs` until the bundle is built is the usual choice. haze
commits `dist/index.html` instead, so `cargo build`, `cargo test` and `cargo install` work on a
machine without Bun (the release builders, the Pi, a contributor who only touches Rust). The
price is a build output in git that could go stale, so the build stamps a hash of every input
into the file and `crates/web/tests/bundle.rs` recomputes it from the sources and fails with the
rebuild command when they differ. `build.rs` still fails clearly if the file is missing.

The client asks `/api/series` for the numbers behind the hover readout and `/api/graph` for the
picture, with one time window shared by every graph on the page, so pan and zoom move them
together. Live updates come over SSE (`/api/events`): each finished step updates the status dots
and redraws the graphs when the range ends at now. egui in WASM was rejected: a large download,
weak text and accessibility, and poor on phones.

Navigation is tags as well as the tree: a target path (`dns/cloudflare`) and any number of tags.
Views (built): the tree with status dots and small graphs, a target page, compare (up to 6
targets overlaid), incidents with the alert log. A share link fixes the range in the URL. Dark by
default with a light switch, and a phone layout. Next: every vantage point of a target overlaid,
a loss heatmap (targets by time), and maintenance windows on the incident list.

The dashboard listens on 127.0.0.1:8480 and has no login; exposing it means a reverse proxy with
authentication until haze has users.

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

Routes: webhook (JSON) and ntfy are built, each filtered by tags, retried twice on server
errors and sent from a queue so a slow endpoint never holds up probing. Next: SMTP,
Telegram, and a severity filter.

## 6. Distributed

Options: SmokePing style HTTP with a shared secret, mTLS, Noise, or signed requests over HTTPS.

Choice: `haze agent` is the same binary in agent mode. It has an Ed25519 key and the server
allowlists agent public keys. The agent fetches its assignment (the targets tagged for it) and pushes batches of step
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
