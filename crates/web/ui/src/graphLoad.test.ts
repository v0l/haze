import { describe, expect, it, mock } from "bun:test";
import { REFRESH_MS, createRefresher, type GraphSpec } from "./graphLoad";

const spec: GraphSpec = {
  names: ["a"],
  compare: false,
  range: { span: 3 * 3600e3, to: null },
  width: 300,
  height: 80,
  theme: "dark",
  small: true,
  withSeries: false,
};

function stub() {
  return {
    svg: mock(async () => "<svg/>"),
    series: mock(async () => ({}) as never),
    graphUrl: mock(() => "/g"),
    compareUrl: mock(() => "/c"),
  };
}

describe("graph refresh", () => {
  it("does not refetch on clock ticks inside one step", async () => {
    const api = stub();
    const r = createRefresher(api);
    const t0 = 10 * REFRESH_MS + 1000;
    expect(await r.refresh(spec, t0)).not.toBeNull();
    for (let i = 1; i < 5; i++) expect(await r.refresh(spec, t0 + i * 10e3)).toBeNull();
    expect(api.svg).toHaveBeenCalledTimes(1);
    expect(api.series).not.toHaveBeenCalled();
    expect(await r.refresh(spec, t0 + REFRESH_MS)).not.toBeNull();
    expect(api.svg).toHaveBeenCalledTimes(2);
  });
});
