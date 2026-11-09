import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { api, subscribe } from "./api";
import { DashContext, type Dash, type Theme } from "./state";
import { defaultRange, parseHash, routeHash, type Range, type Route } from "./route";
import type { LiveEvent, TargetView } from "./types";
import { TreePage } from "./pages/Tree";
import { TargetPage } from "./pages/Target";
import { ComparePage } from "./pages/Compare";
import { IncidentsPage } from "./pages/Incidents";
import icon from "./icon.svg";

const REDRAW_DELAY = 800;
const CLOCK_TICK = 30e3;

function initial(): { route: Route; range: Range } {
  return parseHash(location.hash, defaultRange());
}

const NAV: Array<[Route["page"], string, Route]> = [
  ["tree", "Targets", { page: "tree", tag: null }],
  ["compare", "Compare", { page: "compare", targets: [] }],
  ["incidents", "Incidents", { page: "incidents" }],
];

export function App() {
  const [{ route, range }, setLocation] = useState(initial);
  const [now, setNow] = useState(() => Date.now());
  const [theme, setTheme] = useState<Theme>(() => (document.documentElement.dataset.theme === "light" ? "light" : "dark"));
  const [targets, setTargets] = useState<TargetView[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [live, setLive] = useState(false);
  const [alertsVersion, setAlertsVersion] = useState(0);
  const redraw = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rangeRef = useRef(range);
  rangeRef.current = range;

  const load = useCallback(() => {
    api
      .targets()
      .then((data) => {
        setTargets(data.targets);
        setLoadError(null);
      })
      .catch((e: Error) => setLoadError(e.message));
  }, []);

  useEffect(() => {
    load();
    const onHash = () => setLocation((current) => parseHash(location.hash, current.range));
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [load]);

  const scheduleRedraw = useCallback(() => {
    if (rangeRef.current.to !== null || redraw.current) return;
    redraw.current = setTimeout(() => {
      redraw.current = null;
      setNow(Date.now());
    }, REDRAW_DELAY);
  }, []);

  useEffect(() => {
    if (new URLSearchParams(location.search).has("static")) return;
    const apply = (event: LiveEvent) => {
      setTargets((list) =>
        list.map((t) =>
          t.path !== event.target
            ? t
            : event.kind === "step"
              ? { ...t, status: event.status, last: event.last, backoff: event.backoff }
              : {
                  ...t,
                  status: event.status,
                  active: ["raised", "flapping", "settled-active"].includes(event.record.event)
                    ? [...new Set([...t.active, event.record.alert])]
                    : t.active.filter((a) => a !== event.record.alert),
                },
        ),
      );
      if (event.kind === "alert") setAlertsVersion((v) => v + 1);
      else scheduleRedraw();
    };
    return subscribe({
      onOpen: () => setLive(true),
      onError: () => setLive(false),
      onEvent: apply,
      onResync: () => {
        load();
        setAlertsVersion((v) => v + 1);
        setNow(Date.now());
      },
    });
  }, [load, scheduleRedraw]);

  useEffect(() => {
    const tick = setInterval(() => {
      if (rangeRef.current.to === null) setNow(Date.now());
    }, CLOCK_TICK);
    return () => clearInterval(tick);
  }, []);

  const go = useCallback((next: { route: Route; range: Range }, replace: boolean) => {
    const hash = routeHash(next.route, next.range);
    if (replace) history.replaceState(null, "", hash);
    else history.pushState(null, "", hash);
    setNow(Date.now());
    setLocation(next);
  }, []);

  const dash: Dash = useMemo(
    () => ({
      route,
      range,
      now,
      theme,
      targets,
      alertsVersion,
      setRange: (next) => go({ route, range: next }, true),
      navigate: (next, nextRange) => go({ route: next, range: nextRange ?? range }, false),
    }),
    [route, range, now, theme, targets, alertsVersion, go],
  );

  const switchTheme = () => {
    const next: Theme = theme === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    localStorage.setItem("haze-theme", next);
    setTheme(next);
  };

  const page = (() => {
    if (loadError) return <p class="error">{`Could not load: ${loadError}`}</p>;
    switch (route.page) {
      case "target":
        return <TargetPage key={route.target} path={route.target} />;
      case "compare":
        return <ComparePage chosen={route.targets} />;
      case "incidents":
        return <IncidentsPage />;
      case "tree":
        return <TreePage tag={route.tag} />;
    }
  })();
  const current = route.page === "target" ? "tree" : route.page;

  return (
    <DashContext.Provider value={dash}>
      <header class="bar">
        <a
          class="brand"
          href="#/"
          onClick={(e) => {
            e.preventDefault();
            dash.navigate({ page: "tree", tag: null });
          }}
        >
          <img src={icon} alt="" width={22} height={22} />
          haze
        </a>
        <nav>
          {NAV.map(([id, label, target]) => (
            <a
              key={id}
              href={`#${id === "tree" ? "/" : `/${id}`}`}
              class={current === id ? "on" : ""}
              onClick={(e) => {
                e.preventDefault();
                dash.navigate(target);
              }}
            >
              {label}
            </a>
          ))}
        </nav>
        <span id="live" class={`live${live ? " on" : ""}`} title={live ? "Live updates connected" : "Live updates disconnected"}>
          live
        </span>
        <button id="theme" class="ghost" type="button" aria-label="Switch between dark and light" onClick={switchTheme}>
          {theme === "dark" ? "light" : "dark"}
        </button>
      </header>
      <main id="view">{page}</main>
    </DashContext.Provider>
  );
}
