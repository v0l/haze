import { RANGES, clampRange, windowOf } from "../route";
import { useDash } from "../state";
import { stamp } from "../format";

export function RangeBar() {
  const { range, now, setRange } = useDash();
  const { from, to } = windowOf(range, now);
  const live = range.to === null;
  const shift = (factor: number) => () => setRange(clampRange(range.span, Math.min(now, to + range.span * factor), now));
  const zoom = (factor: number) => () => {
    const mid = (from + to) / 2;
    const span = range.span * factor;
    const end = live && factor > 1 ? null : Math.min(now, mid + span / 2);
    setRange(clampRange(span, end, now));
  };
  return (
    <div class="ranges">
      <div class="group">
        {RANGES.map(([name, span]) => (
          <button key={name} type="button" class={`chip${live && range.span === span ? " on" : ""}`} onClick={() => setRange({ span, to: null })}>
            {name}
          </button>
        ))}
      </div>
      <div class="group">
        <button type="button" class="chip" title="Earlier" aria-label="Earlier" onClick={shift(-0.5)}>
          {"\u2190"}
        </button>
        <button type="button" class="chip" title="Zoom in" aria-label="Zoom in" onClick={zoom(0.5)}>
          +
        </button>
        <button type="button" class="chip" title="Zoom out" aria-label="Zoom out" onClick={zoom(2)}>
          {"\u2212"}
        </button>
        <button type="button" class="chip" title="Later" aria-label="Later" onClick={shift(0.5)}>
          {"\u2192"}
        </button>
        <button type="button" class="chip" title="Back to now" onClick={() => setRange({ span: range.span, to: null })}>
          now
        </button>
      </div>
      <span id="range-label" class="muted small">
        {`${stamp(from)} to ${live ? "now" : stamp(to)}`}
      </span>
    </div>
  );
}
