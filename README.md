# haze

Latency and loss monitoring with smoke graphs, in the spirit of SmokePing. Every step sends a
number of probes spread across the interval, keeps every round trip time, and stores the whole
distribution so a graph of last year shows the same smoke as a graph of the last hour.

## Status

A first skeleton. It runs ICMP, UDP echo, TCP connect and HTTP/HTTPS probes, stores every step,
evaluates alert rules and prints smoke lines in the terminal. There is no web UI, no agent mode
and no notifications yet; `docs/design.md` has the plan.

## Use

```
haze ping 1.1.1.1 -c 20
haze ping https://example.com/ --probe http -c 5
haze ping example.com --probe tcp -p 443
haze check examples/demo.toml
haze run examples/demo.toml
haze show demo-data --last 1h
```

`ping` prints one smoke line: the shaded cells show where the replies fell on a 0 to scale ms
axis, `|` is the median.

```
1.1.1.1 10 pings over 5.0s, scale 0-20.0 ms
[         ░|░      ░       ░             ] loss   0% min 4.52 med 5.31 p90 9.44 max 13.4 jit 1.32 ms
```

ICMP uses unprivileged datagram sockets, so it needs no root. On Linux your group must be inside
`net.ipv4.ping_group_range` (most distributions allow every group); macOS allows it by default.

## Config

One TOML file. A table with `host` or `url` is a target; any other table is a group whose
settings its children inherit. Tags and alerts add up down the tree.

```toml
[defaults]
step = "60s"
pings = 20
timeout = "2s"

[storage]
dir = "/var/lib/haze"

[alerts.loss]
when = "loss >= 10% for 3 steps"

[alerts.slow]
when = "median > 2x baseline 3 of 5"

[targets.dns]
tags = ["dns"]
alerts = ["loss", "slow"]

[targets.dns.cloudflare]
host = "1.1.1.1"

[targets.web.example]
url = "https://example.com/"
pings = 5
```

Per target settings: `probe` (`icmp`, `tcp`, `udp`, `http`), `step` (at least 5 s), `pings`
(1 to 20), `timeout`, `family` (4 or 6), `source`, `size`, `tos`, `port`, `tags`, `alerts`,
`depends`.

Alert rules: a metric (`loss`, `min`, `median`, `mean`, `max`, `p10` to `p99`, `jitter`,
`smoke`), a comparison and a threshold in ms, % or `Nx baseline`, then an optional window
(`for 3 steps`, `3 of 5`). SmokePing patterns work too: `loss pattern ==0%,==0%,>20%,>20%`.

## Build

```
cargo build --release
cargo test
```

## License

GPL-3.0-or-later.
