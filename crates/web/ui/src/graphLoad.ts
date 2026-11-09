import type { api as realApi, GraphQuery } from "./api";
import { windowOf, type Range } from "./route";
import type { Series } from "./types";

export const REFRESH_MS = 60e3;

export interface GraphSpec {
  names: string[];
  compare: boolean;
  range: Range;
  width: number;
  height: number;
  theme: string;
  small: boolean;
  withSeries: boolean;
}

export interface Loaded {
  url: string;
  svg: string;
  series: Promise<Series[]> | null;
}

type Api = Pick<typeof realApi, "svg" | "series" | "graphUrl" | "compareUrl">;

export function refreshBucket(range: Range, now: number): number {
  return range.to === null ? Math.floor(now / REFRESH_MS) : 0;
}

export function specKey(spec: GraphSpec, now: number): string {
  const { names, compare, range, width, height, theme, small, withSeries } = spec;
  return [names.join(","), compare, range.span, range.to, width, height, theme, small, withSeries, refreshBucket(range, now)].join("|");
}

export function createRefresher(api: Api) {
  let last: string | null = null;
  return {
    reset() {
      last = null;
    },
    async refresh(spec: GraphSpec, now: number): Promise<Loaded | null> {
      const key = specKey(spec, now);
      if (key === last) return null;
      last = key;
      const win = windowOf(spec.range, now);
      const query: GraphQuery = { ...win, width: spec.width, height: spec.height, theme: spec.theme, bare: spec.small };
      const url = spec.compare ? api.compareUrl(spec.names, query) : api.graphUrl(spec.names[0]!, query);
      const series = spec.withSeries ? Promise.all(spec.names.map((n) => api.series(n, win, spec.width))) : null;
      series?.catch(() => undefined);
      try {
        const svg = await api.svg(url);
        return { url, svg, series };
      } catch (e) {
        last = null;
        throw e;
      }
    },
  };
}
