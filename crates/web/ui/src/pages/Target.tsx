import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import { Graph } from "../components/Graph";
import { RangeBar } from "../components/RangeBar";
import { BackoffNote, IncidentTable, LastStepTable } from "../components/Tables";
import { Card, StatusDot, TagChips } from "../components/bits";
import { probeLabel } from "../format";
import { shareLink } from "../route";
import { useDash } from "../state";
import type { Incident } from "../types";

export function TargetPage({ path }: { path: string }) {
  const { targets, route, range, navigate, alertsVersion } = useDash();
  const [svgUrl, setSvgUrl] = useState<string | null>(null);
  const [incidents, setIncidents] = useState<Incident[]>([]);
  const [copied, setCopied] = useState(false);
  const t = targets.find((x) => x.path === path);

  useEffect(() => {
    let cancelled = false;
    api
      .alerts(path)
      .then((a) => !cancelled && setIncidents(a.incidents))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [path, alertsVersion]);

  if (!t) {
    return (
      <>
        <h1>{path}</h1>
        <p class="error">This target is not in the config.</p>
        <a
          href="#"
          onClick={(e) => {
            e.preventDefault();
            navigate({ page: "tree", tag: null });
          }}
        >
          Back to targets
        </a>
      </>
    );
  }

  const crumbs = path.split("/");
  const height = Math.min(320, Math.max(200, Math.round(window.innerHeight * 0.38)));
  const open = (url: string | null) => (e: Event) => {
    e.preventDefault();
    if (url) window.open(url, "_blank");
  };

  return (
    <>
      <div class="head">
        <h1>
          <StatusDot status={t.status} />
          <span class="muted">{crumbs.slice(0, -1).map((c) => `${c} / `).join("")}</span>
          {crumbs[crumbs.length - 1]}
        </h1>
        <div class="muted small">
          {t.title ? `${t.title}, ` : ""}
          {`${probeLabel(t)}, ${t.pings} pings every ${Math.round(t.step_ms / 1000)}s`}
        </div>
        <div class="tags">
          <TagChips tags={t.tags} active={null} />
        </div>
        <BackoffNote backoff={t.backoff} />
      </div>
      <RangeBar />
      <Card>
        <Graph target={path} height={height} onDrawn={({ svgUrl }) => setSvgUrl(svgUrl)} />
        <div class="legend small">
          <span class="swatch smoke" />
          smoke: min to max, p10 to p90, p25 to p75
          <span class="swatch median" />
          median, coloured by loss
          <span class="swatch down" />
          all lost
          <span class="swatch refused" />
          all refused
        </div>
        <div class="exports small">
          <a href="#" onClick={open(svgUrl)}>
            SVG
          </a>
          <a href="#" onClick={open(svgUrl?.replace(".svg?", ".png?") ?? null)}>
            PNG
          </a>
          <a
            href="#"
            onClick={(e) => {
              e.preventDefault();
              void navigator.clipboard?.writeText(shareLink(route, range));
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            }}
          >
            {copied ? "copied" : "Share link"}
          </a>
          <a
            href="#"
            onClick={(e) => {
              e.preventDefault();
              navigate({ page: "compare", targets: [path] });
            }}
          >
            Compare
          </a>
        </div>
      </Card>
      <Card title="Last step">
        <LastStepTable last={t.last} />
      </Card>
      {t.active.length ? (
        <Card title="Active alerts">
          <p class="bad">{t.active.join(", ")}</p>
        </Card>
      ) : null}
      <Card title="Incidents">
        <IncidentTable incidents={incidents} showTarget={false} />
      </Card>
    </>
  );
}
