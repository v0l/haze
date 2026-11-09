import type { Point } from "./types";

export const fmt = (v: number | null | undefined): string =>
  v === null || v === undefined ? "-" : v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2);

export const pct = (v: number | null | undefined): string =>
  v === null || v === undefined ? "-" : `${(v * 100).toFixed(v > 0 && v < 0.1 ? 1 : 0)}%`;

export const probeLabel = (t: { probe: string; address?: string }): string =>
  t.address ? `${t.probe} ${t.address}` : t.probe;

const pad = (n: number) => String(n).padStart(2, "0");

export function stamp(ms: number, withDate = true): string {
  const d = new Date(ms);
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (!withDate) return time;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${time}`;
}

export function ago(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 90) return `${s}s ago`;
  if (s < 5400) return `${Math.round(s / 60)}m ago`;
  if (s < 129600) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function duration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 90) return `${s}s`;
  const m = Math.round(ms / 60e3);
  if (m < 60) return `${m}m`;
  if (m < 2880) return `${Math.floor(m / 60)}h ${m % 60}m`;
  return `${Math.round(m / 1440)}d`;
}

export function pointAt(points: Point[], step: number, t: number): Point | null {
  let found: Point | null = null;
  for (const p of points) if (p.t <= t && t < p.t + step) found = p;
  return found;
}
