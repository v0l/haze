use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use haze_core::{AlertEvent, Config, Step};
use haze_store::Store;
use haze_web::{Hub, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

const HOUR: i64 = 3_600_000;

fn config(dir: &std::path::Path) -> Config {
    Config::parse(&format!(
        r#"
[storage]
dir = "{}"

[alerts.loss]
when = "loss >= 50% for 2 steps"
comment = "half the pings lost"

[targets.dns]
tags = ["dns"]
alerts = ["loss"]

[targets.dns.one]
host = "192.0.2.1"

[targets.dns.two]
host = "192.0.2.2"

[targets.web.site]
url = "https://example.com/"
tags = ["web"]
"#,
        dir.display()
    ))
    .unwrap()
}

fn step(start_ms: i64, base_ms: u64, lost: usize) -> Step {
    Step {
        start_ms,
        step_ms: 60_000,
        rtts: (0..20)
            .map(|i| (i >= lost).then(|| Duration::from_micros(base_ms * 1000 + i as u64 * 100)))
            .collect(),
        duplicates: 0,
        reordered: 0,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    hub: Arc<Hub>,
    app: axum::Router,
    now: i64,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let now = haze_web::now_ms();
    let start = (now - 3 * HOUR) / 60_000 * 60_000;
    {
        let mut store = Store::open(dir.path()).unwrap();
        let mut t = start;
        while t < now - 60_000 {
            store.append("dns/one", &step(t, 5, 0)).unwrap();
            store
                .append(
                    "dns/two",
                    &step(t, 20, if t > now - 20 * 60_000 { 20 } else { 0 }),
                )
                .unwrap();
            t += 60_000;
        }
    }
    let hub = Arc::new(Hub::new(config, dir.path(), now).unwrap());
    let app = router(hub.clone()).unwrap();
    Fixture {
        _dir: dir,
        hub,
        app,
        now,
    }
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, kind, body)
}

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let (status, kind, body) = get(app, uri).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{uri}: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(kind.starts_with("application/json"), "{uri}: {kind}");
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn targets_list_the_config_with_live_status() {
    let f = fixture();
    let data = get_json(&f.app, "/api/targets").await;
    let list = data["targets"].as_array().unwrap();
    let paths: Vec<&str> = list.iter().map(|t| t["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["dns/one", "dns/two", "web/site"]);
    let status: Vec<&str> = list.iter().map(|t| t["status"].as_str().unwrap()).collect();
    assert_eq!(status, ["ok", "down", "unknown"], "restored from the store");
    assert_eq!(list[0]["tags"], serde_json::json!(["dns"]));
    assert_eq!(list[2]["probe"], "http");

    f.hub.step_done("dns/one", &step(f.now, 5, 0));
    f.hub.step_done("dns/two", &step(f.now, 20, 20));
    f.hub.step_done("web/site", &step(f.now, 90, 3));
    let data = get_json(&f.app, "/api/targets").await;
    let status: Vec<&str> = data["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert_eq!(status, ["ok", "down", "loss"]);
    assert_eq!(data["targets"][0]["last"]["median_ms"], 5.9);
}

#[tokio::test]
async fn series_merges_steps_into_buckets() {
    let f = fixture();
    let from = f.now - 2 * HOUR;
    let data = get_json(
        &f.app,
        &format!("/api/series/dns/one?from={from}&to={}&step=600000", f.now),
    )
    .await;
    assert_eq!(data["target"], "dns/one");
    assert_eq!(data["step"], 600_000);
    let points = data["points"].as_array().unwrap();
    assert!((11..=13).contains(&points.len()), "{}", points.len());
    let full = &points[3];
    assert_eq!(full["steps"], 10);
    assert_eq!(full["sent"], 200);
    assert_eq!(full["loss"], 0.0);
    assert_eq!(full["min"], 5.0);
    assert_eq!(full["max"], 6.9);
    let median = full["median"].as_f64().unwrap();
    assert!((median - 5.9).abs() <= 5.9 * 0.01 + 0.001, "{median}");

    let lossy = get_json(
        &f.app,
        &format!(
            "/api/series/dns/two?from={}&to={}&step=600000",
            f.now - 30 * 60_000,
            f.now
        ),
    )
    .await;
    let last = lossy["points"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["loss"], 1.0);
    assert!(last["median"].is_null());
}

#[tokio::test]
async fn series_picks_a_step_for_the_range_and_rejects_bad_ranges() {
    let f = fixture();
    let day = get_json(
        &f.app,
        &format!(
            "/api/series/dns/one?from={}&to={}&w=900",
            f.now - 24 * HOUR,
            f.now
        ),
    )
    .await;
    assert_eq!(day["step"], 300_000);
    let year = get_json(
        &f.app,
        &format!(
            "/api/series/dns/one?from={}&to={}&w=900",
            f.now - 400 * 24 * HOUR,
            f.now
        ),
    )
    .await;
    assert_eq!(year["step"], 86_400_000);
    let (status, _, _) = get(
        &f.app,
        &format!("/api/series/dns/one?from={}&to={}", f.now, f.now - 1),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&f.app, "/api/series/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get(&f.app, "/api/series/..%2F..%2Fetc").await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn graphs_are_svg_and_png() {
    let f = fixture();
    let (status, kind, body) = get(&f.app, "/api/graph/dns/one.svg?w=640&h=200").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "image/svg+xml");
    let svg = String::from_utf8(body).unwrap();
    assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
    assert!(svg.contains(r#"width="640""#));
    assert!(svg.contains("#26ff00"), "median line in the no-loss colour");
    assert!(svg.contains("fill-opacity=\"0.22\""), "smoke band");

    let (_, _, body) = get(&f.app, "/api/graph/dns/two.svg?theme=light").await;
    let svg = String::from_utf8(body).unwrap();
    assert!(svg.contains("#e00000"), "fully lost steps are shaded");
    assert!(svg.contains("#ffffff"), "light background");

    let (status, kind, png) = get(&f.app, "/api/graph/dns/one.png?w=400&h=150").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "image/png");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (400, 150));

    let (status, _, _) = get(&f.app, "/api/graph/dns/one.gif").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn compare_overlays_up_to_six_targets() {
    let f = fixture();
    let (status, kind, body) = get(&f.app, "/api/compare/graph.svg?targets=dns/one,dns/two").await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(kind, "image/svg+xml");
    let svg = String::from_utf8(body).unwrap();
    assert!(svg.contains("#4cc9f0") && svg.contains("#f72585"));
    assert!(svg.contains("<title>dns/one, dns/two</title>"));
    let seven = ["dns/one"; 7].join(",");
    let (status, _, _) = get(&f.app, &format!("/api/compare/graph.svg?targets={seven}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&f.app, "/api/compare/graph.svg?targets=dns/one,nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn alerts_become_incidents() {
    let f = fixture();
    f.hub
        .alert("dns/two", "loss", AlertEvent::Raised, f.now - 600_000);
    f.hub
        .alert("dns/one", "loss", AlertEvent::Raised, f.now - 500_000);
    f.hub
        .alert("dns/one", "loss", AlertEvent::Cleared, f.now - 400_000);
    let data = get_json(&f.app, "/api/alerts").await;
    let incidents = data["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0]["target"], "dns/one");
    assert_eq!(incidents[0]["ended_ms"], f.now - 400_000);
    assert_eq!(incidents[1]["target"], "dns/two");
    assert!(incidents[1]["ended_ms"].is_null());
    assert_eq!(incidents[1]["comment"], "half the pings lost");
    assert_eq!(data["history"].as_array().unwrap().len(), 3);
    let only = get_json(&f.app, "/api/alerts?target=dns/two").await;
    assert_eq!(only["incidents"].as_array().unwrap().len(), 1);
    let targets = get_json(&f.app, "/api/targets").await;
    assert_eq!(targets["targets"][1]["active"], serde_json::json!(["loss"]));
}

#[tokio::test]
async fn metrics_are_openmetrics() {
    let f = fixture();
    f.hub.step_done("dns/one", &step(f.now, 5, 2));
    f.hub.alert("dns/two", "loss", AlertEvent::Raised, f.now);
    let (status, kind, body) = get(&f.app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("application/openmetrics-text"));
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("haze_pings_sent_total{target=\"dns/one\"} 20\n"));
    assert!(text.contains("haze_pings_received_total{target=\"dns/one\"} 18\n"));
    assert!(
        text.contains("haze_rtt_seconds_bucket{target=\"dns/one\",le=\"0.0064\"} 13\n"),
        "5.2 to 6.4 ms"
    );
    assert!(text.contains("haze_rtt_seconds_bucket{target=\"dns/one\",le=\"0.0128\"} 18\n"));
    assert!(text.contains("haze_rtt_seconds_bucket{target=\"dns/one\",le=\"0.0032\"} 0\n"));
    assert!(text.contains("haze_step_loss_ratio{target=\"dns/one\"} 0.1\n"));
    assert!(text.contains("haze_alert_active{target=\"dns/two\",alert=\"loss\"} 1\n"));
    assert!(text.ends_with("# EOF\n"));
}

#[tokio::test]
async fn the_dashboard_and_its_assets_are_embedded() {
    let f = fixture();
    let (status, kind, body) = get(&f.app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("text/html"));
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains("<div id=\"app\"></div>"));
    assert!(html.contains("<meta name=\"haze-source\""));
    assert!(
        !html.contains("src=\"http") && !html.contains("href=\"http"),
        "no CDN"
    );
    assert!(
        !html.contains("src=\"/"),
        "no separate script or style requests"
    );
    let (status, kind, _) = get(&f.app, "/icon.svg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "image/svg+xml");
}

#[tokio::test]
async fn events_stream_completed_steps() {
    let f = fixture();
    let response = f
        .app
        .clone()
        .oneshot(Request::get("/api/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let mut seen = String::new();
    let read = async {
        let mut sent = false;
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.unwrap().into_data() {
                seen.push_str(&String::from_utf8_lossy(&data));
            }
            if !sent && seen.contains("event: hello") {
                f.hub.step_done("dns/one", &step(f.now, 5, 0));
                sent = true;
            }
            if seen.contains("event: update") && seen.contains("\"kind\":\"step\"") {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("an update within 5 s");
    assert!(seen.contains("\"target\":\"dns/one\""));
    assert!(seen.contains("\"status\":\"ok\""));
}
