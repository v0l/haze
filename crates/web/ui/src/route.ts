export const RANGES: ReadonlyArray<readonly [string, number]> = [
  ["3h", 3 * 3600e3],
  ["1d", 86400e3],
  ["7d", 7 * 86400e3],
  ["30d", 30 * 86400e3],
  ["400d", 400 * 86400e3],
];
export const MAX_COMPARE = 6;
export const MIN_SPAN = 10 * 60e3;
export const MAX_SPAN = 800 * 86400e3;
export const COLOURS = ["#4cc9f0", "#f72585", "#ffd166", "#80ed99", "#b388ff", "#ff9e40"];

export interface Range {
  span: number;
  to: number | null;
}

export type Route =
  | { page: "tree"; tag: string | null }
  | { page: "target"; target: string }
  | { page: "compare"; targets: string[] }
  | { page: "incidents" };

export function defaultRange(): Range {
  return { span: RANGES[0]![1], to: null };
}

export function parseHash(hash: string, current: Range): { route: Route; range: Range } {
  const text = hash.replace(/^#/, "") || "/";
  const [path = "/", query = ""] = text.split("?");
  const params = new URLSearchParams(query);
  let range = current;
  if (params.has("from") && params.has("to")) {
    const from = Number(params.get("from"));
    const to = Number(params.get("to"));
    if (Number.isFinite(from) && Number.isFinite(to) && to > from) range = { span: to - from, to };
  } else if (params.has("range")) {
    const found = RANGES.find(([name]) => name === params.get("range"));
    if (found) range = { span: found[1], to: null };
  }
  if (path.startsWith("/t/")) {
    return { route: { page: "target", target: path.slice(3).split("/").map(decodeURIComponent).join("/") }, range };
  }
  if (path === "/compare") {
    const targets = (params.get("targets") ?? "").split(",").filter(Boolean).slice(0, MAX_COMPARE);
    return { route: { page: "compare", targets }, range };
  }
  if (path === "/incidents") return { route: { page: "incidents" }, range };
  return { route: { page: "tree", tag: params.get("tag") }, range };
}

export function rangeQuery(range: Range, now = Date.now()): string {
  if (range.to === null) {
    const preset = RANGES.find(([, span]) => span === range.span);
    if (preset) return `range=${preset[0]}`;
  }
  const to = range.to ?? now;
  return `from=${Math.round(to - range.span)}&to=${Math.round(to)}`;
}

export function routePath(route: Route): string {
  switch (route.page) {
    case "target":
      return `/t/${route.target.split("/").map(encodeURIComponent).join("/")}`;
    case "compare":
      return "/compare";
    case "incidents":
      return "/incidents";
    case "tree":
      return "/";
  }
}

export function routeHash(route: Route, range: Range): string {
  const extra: string[] = [rangeQuery(range)];
  if (route.page === "compare" && route.targets.length) extra.push(`targets=${route.targets.join(",")}`);
  if (route.page === "tree" && route.tag) extra.push(`tag=${encodeURIComponent(route.tag)}`);
  return `#${routePath(route)}?${extra.join("&")}`;
}

export function windowOf(range: Range, now: number): { from: number; to: number } {
  const to = range.to ?? now;
  return { from: Math.round(to - range.span), to: Math.round(to) };
}

export function shareLink(route: Route, range: Range, now = Date.now()): string {
  const fixed = { span: range.span, to: range.to ?? now };
  return `${location.origin}${location.pathname}${routeHash(route, fixed)}`;
}

export function clampRange(span: number, to: number | null, now = Date.now()): Range {
  const s = Math.max(MIN_SPAN, Math.min(MAX_SPAN, span));
  if (to === null || to >= now - 1000) return { span: s, to: null };
  return { span: s, to };
}
