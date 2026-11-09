# Changelog

All notable changes to haze are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- ICMP echo over unprivileged datagram sockets, IPv4 and IPv6, with kernel receive timestamps on
  Linux.
- UDP echo, TCP connect and HTTP/HTTPS probes. HTTP reports DNS, connect, TLS and first byte
  times, the status and the days left on the certificate.
- A Nostr relay probe that times a `REQ` to its first `EVENT` or `EOSE`, and a DNS probe.
- Pings spread across each step, with loss, duplicates, reordering and RFC 3550 jitter.
- A log bucket histogram within 1% of exact percentiles that merges across steps.
- A store of one file per target per day, checked record by record so a torn write is dropped,
  with hourly and daily rollups of closed days for long ranges.
- Alert rules like `median > 20ms 3 of 5`, `loss >= 10% for 3 steps`, `smoke > 2x baseline` and
  SmokePing patterns, with clear delays, flap detection and dependencies.
- A multi-arch Docker image at `ghcr.io/v0l/haze` (linux/amd64, linux/arm64), non-root on
  `scratch`, with an ICMP raw socket fallback under `CAP_NET_RAW` and a Compose example.
- Webhook and ntfy notifications, filtered by tag.
- Polite connection probes: per probe default rates (1 a minute for http, nostr and smtp, 3 for
  tcp, 5 for dns), a cap of 6 connections a minute per host unless `allow_aggressive` is set,
  one shared limiter per host and port, backoff on 429, 503, resets and TLS alerts that honours
  `Retry-After` and shows on the dashboard, HEAD requests with a GET fallback, FIN on close and
  jittered timing.
- An smtp probe that reads the banner and says `QUIT`, and a `haze check` warning for bare tcp
  probes to ports 22, 25 and 587.
- A web dashboard served by `haze run` on 127.0.0.1:8480: the target tree with tags and status
  dots, a target page with the smoke graph, ranges from 3 hours to 400 days, pan and zoom on one
  time axis, hover readouts, compare for up to 6 targets, incidents, light and dark themes and a
  phone layout. It updates as steps finish.
- SVG and PNG graph endpoints, a JSON API, server-sent events and OpenMetrics `/metrics`.
- A TOML config with groups that pass settings, tags and alerts to their targets. `haze.toml` is
  the default; `HAZE_CONFIG`, `HAZE_DATA` and `HAZE_LISTEN` override it.
- `haze ping`, `check`, `run` and `show`.
