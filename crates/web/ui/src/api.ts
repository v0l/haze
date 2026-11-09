import type { AlertsResponse, LiveEvent, Series, TargetsResponse } from "./types";

export const encodePath = (path: string): string => path.split("/").map(encodeURIComponent).join("/");

async function json<T>(url: string): Promise<T> {
  const response = await fetch(url);
  if (!response.ok) {
    const body = await response.text();
    let message = `${response.status}`;
    try {
      message = (JSON.parse(body) as { error?: string }).error ?? message;
    } catch {
      message = body || message;
    }
    throw new Error(`${url}: ${message}`);
  }
  return (await response.json()) as T;
}

export interface Window {
  from: number;
  to: number;
}

export interface GraphQuery extends Window {
  step: number;
  width: number;
  height: number;
  theme: string;
  bare: boolean;
}

function graphParams(q: GraphQuery): string {
  const tz = new Date().getTimezoneOffset();
  return `from=${q.from}&to=${q.to}&step=${q.step}&w=${q.width}&h=${q.height}&tz=${tz}&theme=${q.theme}${q.bare ? "&bare=1" : ""}`;
}

export const api = {
  targets: () => json<TargetsResponse>("/api/targets"),
  series: (target: string, w: Window, width: number) =>
    json<Series>(`/api/series/${encodePath(target)}?from=${w.from}&to=${w.to}&w=${width}&tz=${new Date().getTimezoneOffset()}`),
  alerts: (target?: string) => json<AlertsResponse>(target ? `/api/alerts?target=${encodeURIComponent(target)}` : "/api/alerts"),
  graphUrl: (target: string, q: GraphQuery) => `/api/graph/${encodePath(target)}.svg?${graphParams(q)}`,
  compareUrl: (targets: string[], q: GraphQuery) =>
    `/api/compare/graph.svg?targets=${targets.map(encodeURIComponent).join(",")}&${graphParams(q)}`,
  async svg(url: string): Promise<string> {
    const response = await fetch(url);
    const text = await response.text();
    if (!response.ok) throw new Error(text);
    return text;
  },
};

export interface LiveHandlers {
  onOpen: () => void;
  onError: () => void;
  onEvent: (event: LiveEvent) => void;
  onResync: () => void;
}

export function subscribe(handlers: LiveHandlers): () => void {
  const source = new EventSource("/api/events");
  source.addEventListener("hello", handlers.onOpen);
  source.addEventListener("update", (e) => handlers.onEvent(JSON.parse((e as MessageEvent<string>).data) as LiveEvent));
  source.addEventListener("resync", handlers.onResync);
  source.onerror = handlers.onError;
  return () => source.close();
}
