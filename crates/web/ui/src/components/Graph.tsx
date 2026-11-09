import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { api, type GraphQuery } from "../api";
import { fmt, pct, pointAt, stamp } from "../format";
import { RANGES, clampRange, windowOf } from "../route";
import { useDash } from "../state";
import type { Point, Series } from "../types";

type HoverListener = (t: number | null) => void;
const hoverListeners = new Set<HoverListener>();
const dragListeners = new Set<(dx: number) => void>();
const broadcastHover = (t: number | null) => hoverListeners.forEach((l) => l(t));
const broadcastDrag = (dx: number) => dragListeners.forEach((l) => l(dx));

export interface GraphProps {
  target?: string;
  targets?: string[];
  height: number;
  small?: boolean;
  onOpen?: () => void;
  onDrawn?: (info: { series: Series[]; svgUrl: string }) => void;
}

interface Plot {
  from: number;
  to: number;
  left: number;
  right: number;
  width: number;
}

function plotOf(svg: SVGSVGElement): Plot {
  const n = (k: string) => Number(svg.dataset[k] ?? 0);
  return { from: n("from"), to: n("to"), left: n("left"), right: n("right"), width: Number(svg.getAttribute("width")) };
}

function Readout({ point, label }: { point: Point | null | undefined; label: string | null }) {
  if (!point) return <div class="muted">{label ? `${label}: no data` : "no data"}</div>;
  const lost = point.sent > 0 && point.received === 0;
  const refused = lost && point.refused >= point.sent;
  return (
    <div>
      {label ? <b>{label} </b> : null}
      <span class="muted">{stamp(point.t)} </span>
      {refused ? <span class="bad">all refused </span> : lost ? <span class="bad">all lost </span> : null}
      {`min ${fmt(point.min)} med ${fmt(point.median)} p90 ${fmt(point.p90)} max ${fmt(point.max)} ms, loss ${pct(point.loss)} (${point.received}/${point.sent})`}
    </div>
  );
}

export function Graph({ target, targets, height, small = false, onOpen, onDrawn }: GraphProps) {
  const dash = useDash();
  const { range, now, theme, setRange } = dash;
  const box = useRef<HTMLDivElement>(null);
  const holder = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [series, setSeries] = useState<Series[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [hover, setHover] = useState<number | null>(null);
  const [cursorX, setCursorX] = useState<number | null>(null);
  const drag = useRef<{ x: number; from: number; to: number; moved: boolean } | null>(null);
  const names = targets ?? (target ? [target] : []);
  const key = names.join(",");

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setWidth(Math.max(240, Math.round(el.clientWidth)));
    measure();
    const observer = new ResizeObserver(() => measure());
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!width || !names.length) return;
    let cancelled = false;
    const win = windowOf(range, now);
    (async () => {
      try {
        const list = await Promise.all(names.map((n) => api.series(n, win, width)));
        const step = Math.max(...list.map((s) => s.step));
        const query: GraphQuery = { from: list[0]?.from ?? win.from, to: win.to, step, width, height, theme, bare: small };
        const url = targets ? api.compareUrl(names, query) : api.graphUrl(names[0]!, query);
        const svg = await api.svg(url);
        if (cancelled || !holder.current) return;
        holder.current.innerHTML = svg;
        setSeries(list);
        setError(null);
        onDrawn?.({ series: list, svgUrl: url });
      } catch (e) {
        if (!cancelled) setError((e as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [key, width, height, theme, small, range.span, range.to, now]);

  useEffect(() => {
    const onHover: HoverListener = (t) => setHover(t);
    const onDrag = (dx: number) => {
      const svg = holder.current?.querySelector("svg");
      if (svg) svg.style.transform = dx ? `translateX(${dx}px)` : "";
    };
    hoverListeners.add(onHover);
    dragListeners.add(onDrag);
    return () => {
      hoverListeners.delete(onHover);
      dragListeners.delete(onDrag);
    };
  }, []);

  const geometry = (clientX: number) => {
    const svg = holder.current?.querySelector("svg");
    if (!svg) return null;
    const plot = plotOf(svg);
    const rect = svg.getBoundingClientRect();
    const scale = rect.width / plot.width;
    const left = plot.left * scale;
    const inner = rect.width - left - plot.right * scale;
    const ratio = (clientX - rect.left - left) / inner;
    return { ...plot, t: plot.from + ratio * (plot.to - plot.from), inside: ratio >= 0 && ratio <= 1, inner, scale };
  };

  useEffect(() => {
    const svg = holder.current?.querySelector("svg");
    if (hover === null || !svg || !box.current) {
      setCursorX(null);
      return;
    }
    const plot = plotOf(svg);
    const rect = svg.getBoundingClientRect();
    const scale = rect.width / plot.width;
    const inner = rect.width - (plot.left + plot.right) * scale;
    const ratio = (hover - plot.from) / (plot.to - plot.from);
    setCursorX(ratio >= 0 && ratio <= 1 ? plot.left * scale + ratio * inner : null);
  }, [hover, series]);

  const onPointerMove = (event: PointerEvent) => {
    const g = geometry(event.clientX);
    if (!g) return;
    if (drag.current) {
      const dx = event.clientX - drag.current.x;
      if (Math.abs(dx) > 3) drag.current.moved = true;
      broadcastDrag(dx);
      return;
    }
    broadcastHover(g.inside ? g.t : null);
  };

  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const g = geometry(event.clientX);
    if (!g) return;
    drag.current = { x: event.clientX, from: g.from, to: g.to, moved: false };
    box.current?.setPointerCapture(event.pointerId);
    box.current?.classList.add("dragging");
  };

  const onPointerUp = (event: PointerEvent) => {
    const d = drag.current;
    drag.current = null;
    box.current?.classList.remove("dragging");
    broadcastDrag(0);
    if (!d) return;
    if (!d.moved) {
      onOpen?.();
      return;
    }
    const g = geometry(event.clientX);
    const inner = g?.inner ?? 1;
    const dx = event.clientX - d.x;
    const shift = (-dx / inner) * (d.to - d.from);
    setRange(clampRange(d.to - d.from, Math.min(Date.now(), d.to + shift)));
  };

  const onWheel = (event: WheelEvent) => {
    if (small && !event.ctrlKey && !event.altKey) return;
    const g = geometry(event.clientX);
    if (!g || !g.inside) return;
    event.preventDefault();
    const factor = event.deltaY > 0 ? 1.25 : 0.8;
    const span = (g.to - g.from) * factor;
    const ratio = (g.t - g.from) / (g.to - g.from);
    const live = range.to === null && factor > 1;
    setRange(clampRange(span, live ? null : Math.min(Date.now(), g.t + span * (1 - ratio))));
  };

  const last = series.map((s) => s.points[s.points.length - 1]);
  const shown = hover === null ? last : series.map((s) => pointAt(s.points, s.step, hover));

  return (
    <>
      <div
        ref={box}
        class={`graph${small ? " small" : ""}`}
        role="img"
        aria-label={target ? `Smoke graph of ${target}` : "Comparison graph"}
        onPointerMove={onPointerMove}
        onPointerLeave={() => broadcastHover(null)}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onWheel={onWheel}
        onDblClick={() => setRange({ span: RANGES[0]![1], to: null })}
      >
        {error ? <p class="error">{`Could not draw the graph: ${error}`}</p> : null}
        <div ref={holder} class="svg-holder" />
        {cursorX !== null ? <div class="cursor" style={{ left: `${cursorX}px` }} /> : null}
      </div>
      {small ? null : (
        <div class="readout">
          {series.length === 0 ? (
            <span class="muted">Hover the graph for a step</span>
          ) : (
            series.map((s, i) => <Readout key={s.target} point={shown[i]} label={series.length > 1 ? s.target : null} />)
          )}
        </div>
      )}
    </>
  );
}
