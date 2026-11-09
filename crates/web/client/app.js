const view = document.getElementById("view");
const liveBadge = document.getElementById("live");
const themeButton = document.getElementById("theme");

const RANGES = [
  ["3h", 3 * 3600e3],
  ["1d", 86400e3],
  ["7d", 7 * 86400e3],
  ["30d", 30 * 86400e3],
  ["400d", 400 * 86400e3],
];
const MAX_COMPARE = 6;
const COLOURS = ["#4cc9f0", "#f72585", "#ffd166", "#80ed99", "#b388ff", "#ff9e40"];
const MIN_SPAN = 10 * 60e3;

const state = {
  targets: [],
  range: { span: RANGES[0][1], to: null },
  graphs: [],
  route: null,
  compare: [],
  tag: null,
};

const el = (tag, attrs = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
};

const theme = () => document.documentElement.dataset.theme;
const tz = () => new Date().getTimezoneOffset();
const encodePath = (path) => path.split("/").map(encodeURIComponent).join("/");
const fmt = (v) => (v === null || v === undefined ? "-" : v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2));
const pct = (v) => (v === null || v === undefined ? "-" : `${(v * 100).toFixed(v > 0 && v < 0.1 ? 1 : 0)}%`);

function window_() {
  const to = state.range.to ?? Date.now();
  return { from: Math.round(to - state.range.span), to: Math.round(to) };
}

function stamp(ms, withDate = true) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (!withDate) return time;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${time}`;
}

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 90) return `${s}s ago`;
  if (s < 5400) return `${Math.round(s / 60)}m ago`;
  if (s < 129600) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function duration(ms) {
  const m = Math.round(ms / 60e3);
  if (m < 60) return `${m}m`;
  if (m < 2880) return `${Math.floor(m / 60)}h ${m % 60}m`;
  return `${Math.round(m / 1440)}d`;
}

async function json(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status}`);
  return response.json();
}

async function loadTargets() {
  const data = await json("/api/targets");
  state.targets = data.targets;
  return state.targets;
}

function parseRoute() {
  const hash = location.hash.replace(/^#/, "") || "/";
  const [path, query = ""] = hash.split("?");
  const params = new URLSearchParams(query);
  if (params.has("from") && params.has("to")) {
    const from = Number(params.get("from"));
    const to = Number(params.get("to"));
    if (to > from) state.range = { span: to - from, to };
  } else if (params.has("range")) {
    const found = RANGES.find(([name]) => name === params.get("range"));
    if (found) state.range = { span: found[1], to: null };
  }
  if (path.startsWith("/t/")) return { page: "target", target: path.slice(3).split("/").map(decodeURIComponent).join("/") };
  if (path === "/compare") {
    const list = (params.get("targets") || "").split(",").filter(Boolean).slice(0, MAX_COMPARE);
    if (list.length) state.compare = list;
    return { page: "compare" };
  }
  if (path === "/incidents") return { page: "incidents" };
  if (params.has("tag")) state.tag = params.get("tag");
  return { page: "tree" };
}

function rangeQuery() {
  if (state.range.to === null) {
    const preset = RANGES.find(([, span]) => span === state.range.span);
    if (preset) return `range=${preset[0]}`;
  }
  const { from, to } = window_();
  return `from=${from}&to=${to}`;
}

function shareLink() {
  const { from, to } = window_();
  const base = location.hash.replace(/^#/, "").split("?")[0] || "/";
  const extra = state.route?.page === "compare" ? `&targets=${state.compare.join(",")}` : "";
  return `${location.origin}${location.pathname}#${base}?from=${from}&to=${to}${extra}`;
}

function setRange(range) {
  state.range = range;
  const base = location.hash.replace(/^#/, "").split("?")[0] || "/";
  const extra = state.route?.page === "compare" && state.compare.length ? `&targets=${state.compare.join(",")}` : "";
  history.replaceState(null, "", `#${base}?${rangeQuery()}${extra}`);
  document.querySelectorAll("[data-range]").forEach((b) => {
    b.classList.toggle("on", state.range.to === null && Number(b.dataset.span) === state.range.span);
  });
  const label = document.getElementById("range-label");
  if (label) label.textContent = rangeLabel();
  redrawGraphs();
}

function rangeLabel() {
  const { from, to } = window_();
  return `${stamp(from)} to ${state.range.to === null ? "now" : stamp(to)}`;
}

function rangeBar() {
  const buttons = RANGES.map(([name, span]) =>
    el("button", { type: "button", class: "chip" + (state.range.to === null && state.range.span === span ? " on" : ""), "data-range": name, "data-span": span, onclick: () => setRange({ span, to: null }) }, name),
  );
  const step = (factor) => () => {
    const { to } = window_();
    const next = Math.min(Date.now(), to + state.range.span * factor);
    setRange({ span: state.range.span, to: next >= Date.now() - 1000 ? null : next });
  };
  const zoom = (factor) => () => {
    const { from, to } = window_();
    const mid = (from + to) / 2;
    const span = Math.max(MIN_SPAN, Math.min(800 * 86400e3, state.range.span * factor));
    const end = Math.min(Date.now(), mid + span / 2);
    setRange({ span, to: state.range.to === null && factor > 1 ? null : end });
  };
  return el(
    "div",
    { class: "ranges" },
    el("div", { class: "group" }, buttons),
    el(
      "div",
      { class: "group" },
      el("button", { type: "button", class: "chip", title: "Earlier", "aria-label": "Earlier", onclick: step(-0.5) }, "\u2190"),
      el("button", { type: "button", class: "chip", title: "Zoom in", "aria-label": "Zoom in", onclick: zoom(0.5) }, "+"),
      el("button", { type: "button", class: "chip", title: "Zoom out", "aria-label": "Zoom out", onclick: zoom(2) }, "\u2212"),
      el("button", { type: "button", class: "chip", title: "Later", "aria-label": "Later", onclick: step(0.5) }, "\u2192"),
      el("button", { type: "button", class: "chip", title: "Back to now", onclick: () => setRange({ span: state.range.span, to: null }) }, "now"),
    ),
    el("span", { id: "range-label", class: "muted small" }, rangeLabel()),
  );
}

function graphSize(node, height) {
  const width = Math.max(240, Math.round(node.clientWidth || node.parentElement?.clientWidth || 800));
  return { width, height };
}

function readout(point, target) {
  if (!point) return el("span", { class: "muted" }, "no data");
  const lost = point.sent > 0 && point.received === 0;
  return el(
    "span",
    {},
    target ? el("b", {}, target, " ") : null,
    el("span", { class: "muted" }, stamp(point.t), " "),
    lost ? el("span", { class: "bad" }, "all lost ") : null,
    `min ${fmt(point.min)} med ${fmt(point.median)} p90 ${fmt(point.p90)} max ${fmt(point.max)} ms, loss ${pct(point.loss)} (${point.received}/${point.sent})`,
  );
}

function pointAt(series, t) {
  if (!series) return null;
  let found = null;
  for (const p of series.points) {
    if (p.t <= t && t < p.t + series.step) found = p;
  }
  return found;
}

function attachInteraction(graph) {
  const box = graph.box;
  let drag = null;
  const timeAt = (clientX) => {
    const svg = box.querySelector("svg");
    if (!svg) return null;
    const rect = svg.getBoundingClientRect();
    const scale = rect.width / Number(svg.getAttribute("width"));
    const left = Number(svg.dataset.left) * scale;
    const right = Number(svg.dataset.right) * scale;
    const from = Number(svg.dataset.from);
    const to = Number(svg.dataset.to);
    const x = (clientX - rect.left - left) / (rect.width - left - right);
    return { t: from + x * (to - from), x: clientX - rect.left, inside: x >= 0 && x <= 1, from, to, width: rect.width - left - right };
  };
  const cursor = el("div", { class: "cursor", hidden: true });
  box.append(cursor);
  box.addEventListener("pointermove", (event) => {
    const at = timeAt(event.clientX);
    if (!at) return;
    if (drag) {
      const dx = event.clientX - drag.x;
      const shift = (-dx / at.width) * (drag.to - drag.from);
      const to = Math.min(Date.now(), drag.to + shift);
      state.range = { span: drag.to - drag.from, to };
      for (const g of state.graphs) g.preview?.(dx);
      return;
    }
    cursor.hidden = !at.inside;
    cursor.style.left = `${at.x}px`;
    for (const g of state.graphs) g.hover?.(at.inside ? at.t : null);
  });
  box.addEventListener("pointerleave", () => {
    cursor.hidden = true;
    for (const g of state.graphs) g.hover?.(null);
  });
  box.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    const at = timeAt(event.clientX);
    if (!at) return;
    drag = { x: event.clientX, from: at.from, to: at.to, moved: false };
    box.setPointerCapture(event.pointerId);
    box.classList.add("dragging");
  });
  const finish = (event) => {
    if (!drag) return;
    const moved = Math.abs(event.clientX - drag.x) > 3;
    drag = null;
    box.classList.remove("dragging");
    for (const g of state.graphs) g.preview?.(0);
    if (moved) {
      const to = state.range.to;
      setRange({ span: state.range.span, to: to !== null && to >= Date.now() - 1000 ? null : to });
    } else if (graph.onclick) {
      graph.onclick();
    }
  };
  box.addEventListener("pointerup", finish);
  box.addEventListener("pointercancel", finish);
  box.addEventListener(
    "wheel",
    (event) => {
      if (!event.ctrlKey && !event.altKey && graph.small) return;
      const at = timeAt(event.clientX);
      if (!at || !at.inside) return;
      event.preventDefault();
      const factor = event.deltaY > 0 ? 1.25 : 0.8;
      const span = Math.max(MIN_SPAN, Math.min(800 * 86400e3, (at.to - at.from) * factor));
      const ratio = (at.t - at.from) / (at.to - at.from);
      let to = at.t + span * (1 - ratio);
      const live = state.range.to === null && factor > 1;
      to = Math.min(Date.now(), to);
      setRange({ span, to: live || to >= Date.now() - 1000 ? null : to });
    },
    { passive: false },
  );
  box.addEventListener("dblclick", () => setRange({ span: RANGES[0][1], to: null }));
}

function makeGraph({ target, targets, height = 260, small = false, onclick, legend }) {
  const box = el("div", { class: "graph" + (small ? " small" : ""), role: "img", "aria-label": target ? `Smoke graph of ${target}` : "Comparison graph" });
  const info = small ? null : el("div", { class: "readout" }, el("span", { class: "muted" }, "Hover the graph for a step"));
  const graph = { box, info, target, targets, small, onclick, series: [], token: 0 };
  graph.hover = (t) => {
    if (!info) return;
    if (t === null) {
      const last = graph.series.map((s) => s.points[s.points.length - 1]);
      info.replaceChildren(...graph.series.map((s, i) => el("div", {}, readout(last[i], graph.series.length > 1 ? s.target : null))));
      return;
    }
    info.replaceChildren(...graph.series.map((s) => el("div", {}, readout(pointAt(s, t), graph.series.length > 1 ? s.target : null))));
  };
  graph.preview = (dx) => {
    const svg = box.querySelector("svg");
    if (svg) svg.style.transform = dx ? `translateX(${dx}px)` : "";
  };
  graph.draw = async () => {
    const token = ++graph.token;
    const { width, height: h } = graphSize(box, height);
    const { from, to } = window_();
    const common = `from=${from}&to=${to}&w=${width}&tz=${tz()}`;
    try {
      const names = targets ?? [target];
      const series = await Promise.all(names.map((name) => json(`/api/series/${encodePath(name)}?${common}`)));
      if (token !== graph.token) return;
      const step = Math.max(...series.map((s) => s.step));
      const aligned = series[0]?.from ?? from;
      const query = `from=${aligned}&to=${to}&step=${step}&w=${width}&h=${h}&tz=${tz()}&theme=${theme()}${small ? "&bare=1" : ""}`;
      const url = targets ? `/api/compare/graph.svg?targets=${names.map(encodeURIComponent).join(",")}&${query}` : `/api/graph/${encodePath(target)}.svg?${query}`;
      const response = await fetch(url);
      const text = await response.text();
      if (token !== graph.token) return;
      if (!response.ok) throw new Error(text);
      const holder = document.createElement("div");
      holder.innerHTML = text;
      const svg = holder.querySelector("svg");
      box.querySelector("svg")?.remove();
      box.prepend(svg);
      graph.series = series;
      graph.svgUrl = url;
      graph.hover(null);
      legend?.(series);
    } catch (error) {
      box.querySelector("svg")?.remove();
      box.querySelector(".error")?.remove();
      box.prepend(el("p", { class: "error" }, `Could not draw the graph: ${error.message}`));
    }
  };
  attachInteraction(graph);
  state.graphs.push(graph);
  return graph;
}

let redrawTimer = null;
function redrawGraphs() {
  clearTimeout(redrawTimer);
  redrawTimer = setTimeout(() => state.graphs.forEach((g) => g.draw()), 30);
}

function statusDot(status, title) {
  return el("span", { class: `dot ${status}`, title: title || status, "aria-label": status });
}

function tagChips(tags, active) {
  return tags.map((tag) =>
    el("a", { class: "tag" + (tag === active ? " on" : ""), href: tag === active ? "#/" : `#/?tag=${encodeURIComponent(tag)}` }, tag),
  );
}

function treeOf(targets) {
  const root = { children: new Map(), targets: [] };
  for (const t of targets) {
    const parts = t.path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      if (!node.children.has(part)) node.children.set(part, { children: new Map(), targets: [] });
      node = node.children.get(part);
    }
    node.targets.push(t);
  }
  return root;
}

const RANK = { down: 4, alert: 3, loss: 2, ok: 1, unknown: 0 };
function worst(node) {
  let best = "unknown";
  const visit = (n) => {
    for (const t of n.targets) if (RANK[t.status] > RANK[best]) best = t.status;
    n.children.forEach(visit);
  };
  visit(node);
  return best;
}

function targetRow(t) {
  const last = t.last;
  const row = el(
    "li",
    { class: "row", "data-target": t.path },
    el(
      "a",
      { class: "name", href: `#/t/${encodePath(t.path)}?${rangeQuery()}` },
      statusDot(t.status),
      el("span", { class: "label" }, t.path.split("/").pop()),
      el("span", { class: "muted small address" }, `${t.probe} ${t.address}`),
    ),
    el("span", { class: "tags" }, tagChips(t.tags, state.tag)),
    el("span", { class: "stat small" }, last ? `${fmt(last.median_ms)} ms` : "-", el("span", { class: "muted" }, last ? ` ${pct(last.loss)}` : "")),
  );
  const spark = makeGraph({ target: t.path, height: 34, small: true, onclick: () => (location.hash = `#/t/${encodePath(t.path)}?${rangeQuery()}`) });
  row.append(spark.box);
  return row;
}

function renderNode(name, node, depth) {
  const list = el("ul", { class: "tree" });
  for (const t of node.targets) list.append(targetRow(t));
  for (const [child, sub] of node.children) list.append(el("li", {}, renderNode(child, sub, depth + 1)));
  if (!name) return list;
  return el("details", { open: true, class: "group-node" }, el("summary", {}, statusDot(worst(node)), el("span", { class: "label" }, name)), list);
}

async function treePage() {
  const targets = await loadTargets();
  const tags = [...new Set(targets.flatMap((t) => t.tags))].sort();
  const shown = state.tag ? targets.filter((t) => t.tags.includes(state.tag)) : targets;
  const counts = {};
  for (const t of targets) counts[t.status] = (counts[t.status] || 0) + 1;
  const summary = el(
    "div",
    { class: "summary" },
    ["down", "alert", "loss", "ok", "unknown"].filter((s) => counts[s]).map((s) => el("span", { class: "pill" }, statusDot(s), `${counts[s]} ${s}`)),
  );
  view.replaceChildren(
    el("div", { class: "head" }, el("h1", {}, "Targets"), summary),
    rangeBar(),
    tags.length ? el("div", { class: "tagbar" }, el("span", { class: "muted small" }, "Tags"), tagChips(tags, state.tag)) : null,
    shown.length ? renderNode(null, treeOf(shown), 0) : el("p", { class: "muted" }, "No targets in the config."),
  );
  redrawGraphs();
}

function lastStepTable(t) {
  const last = t.last;
  if (!last) return el("p", { class: "muted" }, "No step has finished yet.");
  const cells = [
    ["min", fmt(last.min_ms)],
    ["median", fmt(last.median_ms)],
    ["p90", fmt(last.p90_ms)],
    ["max", fmt(last.max_ms)],
    ["jitter", fmt(last.jitter_ms)],
    ["loss", pct(last.loss)],
  ];
  return el(
    "div",
    { class: "stats" },
    cells.map(([k, v]) => el("div", { class: "stat-cell" }, el("span", { class: "muted small" }, k), el("b", {}, v, k === "loss" ? "" : el("span", { class: "muted small" }, " ms")))),
    el("div", { class: "muted small when" }, `last step ${ago(last.at_ms)}, ${last.received} of ${last.sent} replies`),
  );
}

function incidentTable(incidents, showTarget = true) {
  if (!incidents.length) return el("p", { class: "muted" }, "No incidents.");
  return el(
    "table",
    { class: "incidents" },
    el("thead", {}, el("tr", {}, showTarget ? el("th", {}, "Target") : null, el("th", {}, "Alert"), el("th", {}, "Started"), el("th", {}, "Lasted"), el("th", {}, "State"))),
    el(
      "tbody",
      {},
      incidents.map((i) =>
        el(
          "tr",
          { class: i.ended_ms ? "" : "open" },
          showTarget ? el("td", {}, el("a", { href: `#/t/${encodePath(i.target)}?from=${i.started_ms - 3600e3}&to=${Math.min(Date.now(), (i.ended_ms ?? Date.now()) + 3600e3)}` }, i.target)) : null,
          el("td", {}, i.alert, i.comment ? el("div", { class: "muted small" }, i.comment) : null),
          el("td", {}, stamp(i.started_ms)),
          el("td", {}, duration((i.ended_ms ?? Date.now()) - i.started_ms)),
          el("td", {}, i.ended_ms ? el("span", { class: "muted" }, i.end_reason ?? "cleared") : el("span", { class: "bad" }, i.flapping ? "flapping" : "active")),
        ),
      ),
    ),
  );
}

async function targetPage(path) {
  const targets = await loadTargets();
  const t = targets.find((x) => x.path === path);
  if (!t) {
    view.replaceChildren(el("h1", {}, path), el("p", { class: "error" }, "This target is not in the config."), el("a", { href: "#/" }, "Back to targets"));
    return;
  }
  const graph = makeGraph({ target: path, height: Math.min(320, Math.max(200, Math.round(window.innerHeight * 0.38))) });
  const exports = el(
    "div",
    { class: "exports small" },
    el("a", { href: "#", onclick: (e) => { e.preventDefault(); window.open(graph.svgUrl, "_blank"); } }, "SVG"),
    el("a", { href: "#", onclick: (e) => { e.preventDefault(); window.open(graph.svgUrl.replace(".svg?", ".png?"), "_blank"); } }, "PNG"),
    el("a", { href: "#", onclick: (e) => { e.preventDefault(); navigator.clipboard?.writeText(shareLink()); e.target.textContent = "copied"; setTimeout(() => (e.target.textContent = "Share link"), 1500); } }, "Share link"),
    el("a", { href: `#/compare?targets=${encodeURIComponent(path)}&${rangeQuery()}` }, "Compare"),
  );
  const alerts = await json(`/api/alerts?target=${encodeURIComponent(path)}`);
  const crumbs = path.split("/");
  view.replaceChildren(
    el(
      "div",
      { class: "head" },
      el("h1", {}, statusDot(t.status), el("span", { class: "muted" }, crumbs.slice(0, -1).map((c) => `${c} / `).join("")), crumbs[crumbs.length - 1]),
      el("div", { class: "muted small" }, t.title ? `${t.title}, ` : "", `${t.probe} ${t.address}, ${t.pings} pings every ${Math.round(t.step_ms / 1000)}s`),
      el("div", { class: "tags" }, tagChips(t.tags, null)),
    ),
    rangeBar(),
    el("section", { class: "card" }, graph.box, graph.info, el("div", { class: "legend small" }, el("span", { class: "swatch smoke" }), "smoke: min to max, p10 to p90, p25 to p75", el("span", { class: "swatch median" }), "median, coloured by loss", el("span", { class: "swatch down" }), "all lost"), exports),
    el("section", { class: "card" }, el("h2", {}, "Last step"), lastStepTable(t)),
    t.active.length ? el("section", { class: "card" }, el("h2", {}, "Active alerts"), el("p", { class: "bad" }, t.active.join(", "))) : null,
    el("section", { class: "card" }, el("h2", {}, "Incidents"), incidentTable(alerts.incidents, false)),
  );
  redrawGraphs();
}

async function comparePage() {
  const targets = await loadTargets();
  state.compare = state.compare.filter((p) => targets.some((t) => t.path === p));
  if (!state.compare.length) state.compare = targets.slice(0, Math.min(3, targets.length)).map((t) => t.path);
  const legendBox = el("div", { class: "legend small" });
  const graph = makeGraph({
    targets: state.compare,
    height: Math.min(340, Math.max(220, Math.round(window.innerHeight * 0.4))),
    legend: (series) => legendBox.replaceChildren(...series.map((s, i) => el("span", { class: "key" }, el("span", { class: "swatch", style: `background:${COLOURS[i]}` }), s.target))),
  });
  const pick = el(
    "fieldset",
    { class: "picker" },
    el("legend", {}, `Targets (up to ${MAX_COMPARE})`),
    targets.map((t) => {
      const checked = state.compare.includes(t.path);
      return el(
        "label",
        { class: "pick" },
        el("input", {
          type: "checkbox",
          checked,
          disabled: !checked && state.compare.length >= MAX_COMPARE,
          onchange: (e) => {
            if (e.target.checked) state.compare = [...state.compare, t.path].slice(0, MAX_COMPARE);
            else state.compare = state.compare.filter((p) => p !== t.path);
            history.replaceState(null, "", `#/compare?targets=${state.compare.join(",")}&${rangeQuery()}`);
            route();
          },
        }),
        statusDot(t.status),
        t.path,
      );
    }),
  );
  view.replaceChildren(
    el("div", { class: "head" }, el("h1", {}, "Compare")),
    rangeBar(),
    state.compare.length ? el("section", { class: "card" }, graph.box, legendBox, graph.info) : el("p", { class: "muted" }, "Pick a target."),
    pick,
  );
  if (state.compare.length) redrawGraphs();
  else state.graphs = [];
}

async function incidentsPage() {
  const data = await json("/api/alerts");
  view.replaceChildren(
    el("div", { class: "head" }, el("h1", {}, "Incidents"), el("span", { class: "muted small" }, `${data.incidents.filter((i) => !i.ended_ms).length} active`)),
    el("section", { class: "card" }, incidentTable(data.incidents)),
    el(
      "section",
      { class: "card" },
      el("h2", {}, "Alert log"),
      data.history.length
        ? el("ul", { class: "log small" }, data.history.slice(0, 100).map((r) => el("li", {}, el("span", { class: "muted" }, stamp(r.at_ms), " "), el("b", {}, r.target), ` ${r.alert} ${r.event}`)))
        : el("p", { class: "muted" }, "Nothing yet."),
    ),
  );
}

async function route() {
  state.graphs = [];
  state.route = parseRoute();
  document.querySelectorAll("[data-nav]").forEach((a) => {
    const page = state.route.page === "target" ? "tree" : state.route.page;
    a.classList.toggle("on", a.dataset.nav === page);
  });
  try {
    if (state.route.page === "target") await targetPage(state.route.target);
    else if (state.route.page === "compare") await comparePage();
    else if (state.route.page === "incidents") await incidentsPage();
    else await treePage();
  } catch (error) {
    view.replaceChildren(el("p", { class: "error" }, `Could not load: ${error.message}`));
  }
}

function applyUpdate(update) {
  const t = state.targets.find((x) => x.path === update.target);
  if (t) {
    t.status = update.status;
    if (update.last) t.last = update.last;
  }
  document.querySelectorAll(`[data-target="${CSS.escape(update.target)}"] .dot`).forEach((d) => {
    d.className = `dot ${update.status}`;
    d.title = update.status;
  });
  const page = state.route?.page;
  if (update.kind === "alert" && page === "incidents") {
    route();
    return;
  }
  if (state.range.to !== null) return;
  const affected = state.graphs.filter((g) => g.target === update.target || g.targets?.includes(update.target));
  if (page === "target" && state.route.target === update.target) {
    route();
    return;
  }
  affected.forEach((g) => g.draw());
}

function connect() {
  const source = new EventSource("/api/events");
  source.addEventListener("hello", () => {
    liveBadge.classList.add("on");
    liveBadge.title = "Live updates connected";
  });
  source.addEventListener("update", (e) => applyUpdate(JSON.parse(e.data)));
  source.addEventListener("resync", () => route());
  source.onerror = () => {
    liveBadge.classList.remove("on");
    liveBadge.title = "Live updates disconnected, retrying";
  };
}

function syncThemeButton() {
  themeButton.textContent = theme() === "dark" ? "light" : "dark";
}

themeButton.addEventListener("click", () => {
  const next = theme() === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  localStorage.setItem("haze-theme", next);
  syncThemeButton();
  redrawGraphs();
});

let resizeTimer = null;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(redrawGraphs, 150);
});
window.addEventListener("hashchange", route);
setInterval(() => {
  if (state.range.to === null && state.graphs.length) {
    const label = document.getElementById("range-label");
    if (label) label.textContent = rangeLabel();
  }
}, 30e3);

syncThemeButton();
connect();
route();
