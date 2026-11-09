import { useState } from "preact/hooks";
import { Graph } from "../components/Graph";
import { RangeBar } from "../components/RangeBar";
import { Card, StatusDot } from "../components/bits";
import { COLOURS, MAX_COMPARE } from "../route";
import { useDash } from "../state";
import type { Series } from "../types";

export function ComparePage({ chosen }: { chosen: string[] }) {
  const { targets, navigate } = useDash();
  const [drawn, setDrawn] = useState<Series[]>([]);
  const known = chosen.filter((p) => targets.some((t) => t.path === p));
  const list = known.length || chosen.length ? known : targets.slice(0, Math.min(3, targets.length)).map((t) => t.path);
  const height = Math.min(340, Math.max(220, Math.round(window.innerHeight * 0.4)));
  const toggle = (path: string, on: boolean) =>
    navigate({ page: "compare", targets: on ? [...list, path].slice(0, MAX_COMPARE) : list.filter((p) => p !== path) });

  return (
    <>
      <div class="head">
        <h1>Compare</h1>
      </div>
      <RangeBar />
      {list.length ? (
        <Card>
          <Graph key={list.join(",")} targets={list} height={height} onDrawn={({ series }) => setDrawn(series)} />
          <div class="legend small">
            {drawn.map((s, i) => (
              <span class="key" key={s.target}>
                <span class="swatch" style={{ background: COLOURS[i] }} />
                {s.target}
              </span>
            ))}
          </div>
        </Card>
      ) : (
        <p class="muted">Pick a target.</p>
      )}
      <fieldset class="picker">
        <legend>{`Targets (up to ${MAX_COMPARE})`}</legend>
        {targets.map((t) => {
          const checked = list.includes(t.path);
          return (
            <label class="pick" key={t.path}>
              <input
                type="checkbox"
                checked={checked}
                disabled={!checked && list.length >= MAX_COMPARE}
                onChange={(e) => toggle(t.path, (e.target as HTMLInputElement).checked)}
              />
              <StatusDot status={t.status} />
              {t.path}
            </label>
          );
        })}
      </fieldset>
    </>
  );
}
