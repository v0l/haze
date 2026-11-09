pub mod graph;
pub mod hub;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::stream::{self, Stream};
use haze_store::Store;
use serde::{Deserialize, Serialize};

use crate::graph::{Frame, GraphOptions, MAX_COMPARE, Series, Theme};
pub use crate::hub::Hub;

const DEFAULT_SPAN: i64 = 3 * 3_600_000;
const MAX_SPAN: i64 = 800 * 86_400_000;
const MAX_POINTS: i64 = 4000;
const RECENT_ALERTS: usize = 500;

static INDEX: &str = include_str!("../ui/dist/index.html");
static ICON: &str = include_str!("../ui/src/icon.svg");

#[derive(Clone)]
pub struct App {
    hub: Arc<Hub>,
    store: Arc<Store>,
    addresses: Arc<BTreeMap<String, String>>,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub fn router(hub: Arc<Hub>) -> std::io::Result<Router> {
    let store = Arc::new(Store::open(hub.config().storage_dir.clone())?);
    let config = hub.config();
    let addresses = config
        .targets
        .iter()
        .filter_map(|t| {
            let shown = config.shown_address(t, haze_core::address::resolve_system)?;
            Some((t.path.clone(), shown))
        })
        .collect();
    let app = App {
        hub,
        store,
        addresses: Arc::new(addresses),
    };
    Ok(Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route("/icon.svg", get(|| async { asset("image/svg+xml", ICON) }))
        .route("/api/targets", get(targets))
        .route("/api/series/{*target}", get(series))
        .route("/api/graph/{*file}", get(graph))
        .route("/api/compare/{file}", get(compare))
        .route("/api/alerts", get(alerts))
        .route("/api/events", get(events))
        .route("/metrics", get(metrics))
        .with_state(app))
}

fn asset(kind: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError(status, message.into())
}

type Reply = Result<Response, ApiError>;

#[derive(Serialize)]
struct TargetView {
    path: String,
    title: Option<String>,
    probe: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    tags: Vec<String>,
    alerts: Vec<String>,
    depends: Vec<String>,
    step_ms: u64,
    pings: u32,
    status: &'static str,
    active: Vec<String>,
    last: Option<hub::LastStep>,
    backoff: Option<hub::BackingOff>,
}

async fn targets(State(app): State<App>) -> impl IntoResponse {
    let list: Vec<TargetView> = app
        .hub
        .config()
        .targets
        .iter()
        .map(|t| TargetView {
            path: t.path.clone(),
            title: t.title.clone(),
            probe: t.probe.name().to_string(),
            address: app.addresses.get(&t.path).cloned(),
            tags: t.tags.clone(),
            alerts: t.alerts.clone(),
            depends: t.depends.clone(),
            step_ms: t.step.as_millis() as u64,
            pings: t.pings,
            status: app.hub.status(&t.path),
            active: app.hub.active(&t.path),
            last: app.hub.last(&t.path),
            backoff: app.hub.backoff(&t.path),
        })
        .collect();
    axum::Json(serde_json::json!({ "targets": list, "now": now_ms() }))
}

#[derive(Debug, Default, Deserialize)]
pub struct Range {
    from: Option<i64>,
    to: Option<i64>,
    step: Option<i64>,
    w: Option<u32>,
    h: Option<u32>,
    theme: Option<String>,
    tz: Option<i64>,
    bare: Option<u8>,
    targets: Option<String>,
}

struct Window {
    from: i64,
    to: i64,
    step: i64,
}

impl App {
    fn known(&self, target: &str) -> Result<(), ApiError> {
        if haze_store::check_path(target).is_err() {
            return Err(error(
                StatusCode::BAD_REQUEST,
                format!("`{target}` is not a target path"),
            ));
        }
        let configured = self.hub.config().target(target).is_some();
        let stored = self.store.root().join(target).is_dir();
        if !(configured || stored) {
            return Err(error(
                StatusCode::NOT_FOUND,
                format!("no target `{target}`"),
            ));
        }
        Ok(())
    }

    fn min_step(&self, target: &str) -> i64 {
        self.hub
            .config()
            .target(target)
            .map(|t| t.step.as_millis() as i64)
            .unwrap_or(60_000)
    }

    fn window(&self, range: &Range, min_step: i64) -> Result<Window, ApiError> {
        let to = range.to.unwrap_or_else(now_ms);
        let from = range.from.unwrap_or(to - DEFAULT_SPAN);
        if from >= to {
            return Err(error(StatusCode::BAD_REQUEST, "from must be before to"));
        }
        if to - from > MAX_SPAN {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "the range is longer than 800 days",
            ));
        }
        let floor = ((to - from) / MAX_POINTS).max(1000);
        let step = match range.step {
            Some(step) => step.max(floor),
            None => graph::choose_step(to - from, range.w.unwrap_or(900), min_step).max(floor),
        };
        Ok(Window { from, to, step })
    }

    async fn series(&self, target: &str, window: &Window) -> Result<Series, ApiError> {
        let store = self.store.clone();
        let target = target.to_string();
        let (from, to, step) = (window.from, window.to, window.step);
        tokio::task::spawn_blocking(move || graph::series(&store, &target, from, to, step))
            .await
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    }
}

fn graph_options(range: &Range) -> GraphOptions {
    let (w, h) = (range.w.unwrap_or(900), range.h.unwrap_or(260));
    let bare = range.bare.unwrap_or(0) == 1;
    GraphOptions {
        frame: if bare {
            Frame::bare(w, h)
        } else {
            Frame::new(w, h)
        },
        theme: Theme::parse(range.theme.as_deref()),
        offset_min: range.tz.unwrap_or(0).clamp(-14 * 60, 14 * 60),
        bare,
    }
}

fn image(file: &str, svg: String) -> Reply {
    if file.ends_with(".png") {
        let png = graph::png(&svg).map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        return Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        svg,
    )
        .into_response())
}

fn split_image(file: &str) -> Option<&str> {
    file.strip_suffix(".svg")
        .or_else(|| file.strip_suffix(".png"))
}

async fn series(
    State(app): State<App>,
    Path(target): Path<String>,
    Query(range): Query<Range>,
) -> Reply {
    app.known(&target)?;
    let window = app.window(&range, app.min_step(&target))?;
    Ok(axum::Json(app.series(&target, &window).await?).into_response())
}

async fn graph(
    State(app): State<App>,
    Path(file): Path<String>,
    Query(range): Query<Range>,
) -> Reply {
    let target = split_image(&file)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "graphs end in .svg or .png"))?;
    app.known(target)?;
    let window = app.window(&range, app.min_step(target))?;
    let series = app.series(target, &window).await?;
    image(&file, graph::smoke_svg(&series, &graph_options(&range)))
}

async fn compare(
    State(app): State<App>,
    Path(file): Path<String>,
    Query(range): Query<Range>,
) -> Reply {
    if split_image(&file) != Some("graph") {
        return Err(error(
            StatusCode::NOT_FOUND,
            "use /api/compare/graph.svg or graph.png",
        ));
    }
    let names: Vec<String> = range
        .targets
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() || names.len() > MAX_COMPARE {
        return Err(error(
            StatusCode::BAD_REQUEST,
            format!("compare 1 to {MAX_COMPARE} targets"),
        ));
    }
    for name in &names {
        app.known(name)?;
    }
    let min_step = names
        .iter()
        .map(|n| app.min_step(n))
        .max()
        .unwrap_or(60_000);
    let window = app.window(&range, min_step)?;
    let mut list = Vec::new();
    for name in &names {
        list.push(app.series(name, &window).await?);
    }
    image(&file, graph::compare_svg(&list, &graph_options(&range)))
}

#[derive(Deserialize)]
struct AlertQuery {
    target: Option<String>,
}

async fn alerts(State(app): State<App>, Query(query): Query<AlertQuery>) -> impl IntoResponse {
    let mut history = app.hub.history();
    if let Some(target) = &query.target {
        history.retain(|r| &r.target == target);
    }
    let incidents = hub::incidents(&history);
    let recent: Vec<_> = history.iter().rev().take(RECENT_ALERTS).cloned().collect();
    axum::Json(serde_json::json!({ "incidents": incidents, "history": recent }))
}

async fn events(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = app.hub.subscribe();
    let hello = Event::default().event("hello").data(now_ms().to_string());
    let updates = stream::unfold(receiver, |mut rx| async move {
        let event = match rx.recv().await {
            Ok(data) => Event::default().event("update").data(data),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                Event::default().event("resync").data("")
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(event), rx))
    });
    let stream = futures_util::StreamExt::chain(stream::iter([Ok(hello)]), updates);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn metrics(State(app): State<App>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        app.hub.metrics(),
    )
}
