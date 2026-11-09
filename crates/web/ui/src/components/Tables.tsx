import { ago, duration, fmt, pct, stamp } from "../format";
import { useDash } from "../state";
import type { Incident, LastStep } from "../types";

export function LastStepTable({ last }: { last: LastStep | null }) {
  if (!last) return <p class="muted">No step has finished yet.</p>;
  const cells: Array<[string, string, boolean]> = [
    ["min", fmt(last.min_ms), true],
    ["median", fmt(last.median_ms), true],
    ["p90", fmt(last.p90_ms), true],
    ["max", fmt(last.max_ms), true],
    ["jitter", fmt(last.jitter_ms), true],
    ["loss", pct(last.loss), false],
    ["refused", String(last.refused), false],
  ];
  return (
    <div class="stats">
      {cells.map(([k, v, unit]) => (
        <div class="stat-cell" key={k}>
          <span class="muted small">{k}</span>
          <b>
            {v}
            {unit ? <span class="muted small"> ms</span> : null}
          </b>
        </div>
      ))}
      <div class="muted small when">{`last step ${ago(last.at_ms)}, ${last.received} of ${last.sent} replies${last.refused ? `, ${last.refused} refused by the server` : ""}`}</div>
    </div>
  );
}

export function IncidentTable({ incidents, showTarget = true }: { incidents: Incident[]; showTarget?: boolean }) {
  const { navigate } = useDash();
  if (!incidents.length) return <p class="muted">No incidents.</p>;
  const now = Date.now();
  return (
    <table class="incidents">
      <thead>
        <tr>
          {showTarget ? <th>Target</th> : null}
          <th>Alert</th>
          <th>Started</th>
          <th>Lasted</th>
          <th>State</th>
        </tr>
      </thead>
      <tbody>
        {incidents.map((i) => (
          <tr class={i.ended_ms ? "" : "open"} key={`${i.target}-${i.alert}-${i.started_ms}`}>
            {showTarget ? (
              <td>
                <a
                  href="#"
                  onClick={(e) => {
                    e.preventDefault();
                    const to = Math.min(now, (i.ended_ms ?? now) + 3600e3);
                    const from = i.started_ms - 3600e3;
                    navigate({ page: "target", target: i.target }, { span: to - from, to: to >= now - 1000 ? null : to });
                  }}
                >
                  {i.target}
                </a>
              </td>
            ) : null}
            <td>
              {i.alert}
              {i.comment ? <div class="muted small">{i.comment}</div> : null}
            </td>
            <td>{stamp(i.started_ms)}</td>
            <td>{duration((i.ended_ms ?? now) - i.started_ms)}</td>
            <td>
              {i.ended_ms ? (
                <span class="muted">{i.end_reason ?? "cleared"}</span>
              ) : (
                <span class="bad">{i.flapping ? "flapping" : "active"}</span>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
