import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import { IncidentTable } from "../components/Tables";
import { Card } from "../components/bits";
import { stamp } from "../format";
import { useDash } from "../state";
import type { AlertsResponse } from "../types";

export function IncidentsPage() {
  const { alertsVersion } = useDash();
  const [data, setData] = useState<AlertsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .alerts()
      .then((d) => !cancelled && setData(d))
      .catch((e: Error) => !cancelled && setError(e.message));
    return () => {
      cancelled = true;
    };
  }, [alertsVersion]);

  if (error) return <p class="error">{`Could not load: ${error}`}</p>;
  if (!data) return <p class="muted">Loading</p>;
  return (
    <>
      <div class="head">
        <h1>Incidents</h1>
        <span class="muted small">{`${data.incidents.filter((i) => !i.ended_ms).length} active`}</span>
      </div>
      <Card>
        <IncidentTable incidents={data.incidents} />
      </Card>
      <Card title="Alert log">
        {data.history.length ? (
          <ul class="log small">
            {data.history.slice(0, 100).map((r, i) => (
              <li key={i}>
                <span class="muted">{`${stamp(r.at_ms)} `}</span>
                <b>{r.target}</b>
                {` ${r.alert} ${r.event}`}
              </li>
            ))}
          </ul>
        ) : (
          <p class="muted">Nothing yet.</p>
        )}
      </Card>
    </>
  );
}
