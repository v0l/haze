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
- Pings spread across each step, with loss, duplicates, reordering and RFC 3550 jitter.
- A log bucket histogram within 1% of exact percentiles that merges across steps.
- A store of one file per target per day, checked record by record so a torn write is dropped.
- Alert rules like `median > 20ms 3 of 5`, `loss >= 10% for 3 steps`, `smoke > 2x baseline` and
  SmokePing patterns, with clear delays, flap detection and dependencies.
- A TOML config with groups that pass settings, tags and alerts to their targets.
- `haze ping`, `check`, `run` and `show`.
