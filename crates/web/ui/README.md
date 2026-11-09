# Dashboard

The haze web dashboard: Preact and TypeScript, built by Vite with `vite-plugin-singlefile` into
one HTML file, `dist/index.html`, that `crates/web/src/lib.rs` embeds with `include_str!`.
Shipping the dashboard is shipping the binary.

```bash
bun install
bun run dev      # http://localhost:5173, proxying /api and /metrics to haze run on :8480
HAZE_URL=http://10.0.0.5:8480 bun run dev
bun run check    # tsc
bun run build    # tsc, then dist/index.html
```

`dist/index.html` is committed so `cargo build` works without Bun. The build stamps it with a
hash of everything here that goes into it (`index.html`, `package.json`, `bun.lock`, the configs
and `src/`), and `crates/web/tests/bundle.rs` fails when the stamp does not match the sources,
with the command to run. After changing anything here, run `bun run build` and commit `dist`
with the change.

The graphs are drawn by the server (`/api/graph/<target>.svg`); the client fetches them, the
`/api/series` numbers behind the hover readout, and the live events from `/api/events`. The
response types in `src/types.ts` mirror the Rust JSON in `crates/web/src`.
