use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use haze_core::{AlertEvent, Config, Step, Summary};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub const HISTORY_FILE: &str = "alerts.jsonl";
const RESUME_WINDOW_MS: i64 = 86_400_000;
pub const METRIC_BUCKETS: [f64; 20] = [
    5e-05, 0.0001, 0.0002, 0.0004, 0.0008, 0.0016, 0.0032, 0.0064, 0.0128, 0.0256, 0.0512, 0.1024,
    0.2048, 0.4096, 0.8192, 1.6384, 3.2768, 6.5536, 13.1072, 26.2144,
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AlertRecord {
    pub at_ms: i64,
    pub target: String,
    pub alert: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Incident {
    pub target: String,
    pub alert: String,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub flapping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LastStep {
    pub at_ms: i64,
    pub sent: u32,
    pub received: u32,
    pub loss: f64,
    pub min_ms: Option<f64>,
    pub median_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
}

#[derive(Default)]
struct Live {
    last: Option<LastStep>,
    active: BTreeSet<String>,
    sent: u64,
    received: u64,
    buckets: [u64; METRIC_BUCKETS.len()],
    sum_seconds: f64,
}

pub struct Hub {
    config: Config,
    data_dir: PathBuf,
    live: RwLock<BTreeMap<String, Live>>,
    history: Mutex<Vec<AlertRecord>>,
    log: Mutex<Option<File>>,
    events: broadcast::Sender<String>,
}

pub fn ms(d: Option<Duration>) -> Option<f64> {
    d.map(|d| (d.as_secs_f64() * 1e6).round() / 1e3)
}

impl LastStep {
    pub fn of(step: &Step, summary: &Summary) -> LastStep {
        LastStep {
            at_ms: step.start_ms,
            sent: summary.sent,
            received: summary.received,
            loss: if summary.sent == 0 {
                0.0
            } else {
                (summary.sent - summary.received) as f64 / summary.sent as f64
            },
            min_ms: ms(summary.min),
            median_ms: ms(summary.median),
            p90_ms: ms(summary.p90),
            max_ms: ms(summary.max),
            jitter_ms: ms(summary.jitter),
        }
    }
}

pub fn event_name(event: AlertEvent) -> &'static str {
    match event {
        AlertEvent::Raised => "raised",
        AlertEvent::Cleared => "cleared",
        AlertEvent::Flapping => "flapping",
        AlertEvent::Settled { active: true } => "settled-active",
        AlertEvent::Settled { active: false } => "settled-clear",
    }
}

pub fn incidents(records: &[AlertRecord]) -> Vec<Incident> {
    let mut open: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut list: Vec<Incident> = Vec::new();
    for r in records {
        let key = (r.target.clone(), r.alert.clone());
        match r.event.as_str() {
            "raised" | "flapping" | "settled-active" => {
                if let Some(i) = open.get(&key) {
                    if r.event == "flapping" {
                        list[*i].flapping = true;
                    }
                    continue;
                }
                open.insert(key, list.len());
                list.push(Incident {
                    target: r.target.clone(),
                    alert: r.alert.clone(),
                    started_ms: r.at_ms,
                    ended_ms: None,
                    flapping: r.event == "flapping",
                    comment: r.comment.clone(),
                    end_reason: None,
                });
            }
            _ => {
                if let Some(i) = open.remove(&key) {
                    list[i].ended_ms = Some(r.at_ms);
                    if r.event == "restart" {
                        list[i].end_reason = Some("haze restarted".into());
                    }
                }
            }
        }
    }
    list.reverse();
    list
}

fn read_history(path: &Path) -> Vec<AlertRecord> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

impl Hub {
    pub fn new(config: Config, data_dir: impl Into<PathBuf>, now_ms: i64) -> std::io::Result<Hub> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)?;
        let path = data_dir.join(HISTORY_FILE);
        let mut history = read_history(&path);
        let mut log = OpenOptions::new().create(true).append(true).open(&path)?;
        for incident in incidents(&history) {
            if incident.ended_ms.is_none() {
                let record = AlertRecord {
                    at_ms: now_ms,
                    target: incident.target,
                    alert: incident.alert,
                    event: "restart".into(),
                    comment: None,
                };
                writeln!(log, "{}", serde_json::to_string(&record)?)?;
                history.push(record);
            }
        }
        let mut live: BTreeMap<String, Live> = config
            .targets
            .iter()
            .map(|t| (t.path.clone(), Live::default()))
            .collect();
        if let Ok(store) = haze_store::Store::open(&config.storage_dir) {
            for (path, entry) in live.iter_mut() {
                let recent = store.steps(path, now_ms - RESUME_WINDOW_MS, now_ms + 1);
                if let Some(step) = recent.ok().and_then(|s| s.into_iter().last()) {
                    entry.last = Some(LastStep::of(&step, &step.summary()));
                }
            }
        }
        let (events, _) = broadcast::channel(256);
        Ok(Hub {
            config,
            data_dir,
            live: RwLock::new(live),
            history: Mutex::new(history),
            log: Mutex::new(Some(log)),
            events,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }

    pub fn step_done(&self, target: &str, step: &Step) {
        let summary = step.summary();
        let last = LastStep::of(step, &summary);
        {
            let mut live = self.live.write().unwrap();
            let entry = live.entry(target.to_string()).or_default();
            entry.sent += summary.sent as u64;
            entry.received += summary.received as u64;
            for rtt in step.rtts.iter().flatten() {
                let s = rtt.as_secs_f64();
                entry.sum_seconds += s;
                for (i, le) in METRIC_BUCKETS.iter().enumerate() {
                    if s <= *le {
                        entry.buckets[i] += 1;
                    }
                }
            }
            entry.last = Some(last.clone());
        }
        let event = serde_json::json!({
            "kind": "step",
            "target": target,
            "status": self.status(target),
            "last": last,
        });
        let _ = self.events.send(event.to_string());
    }

    pub fn alert(&self, target: &str, alert: &str, event: AlertEvent, at_ms: i64) {
        let name = event_name(event);
        {
            let mut live = self.live.write().unwrap();
            let entry = live.entry(target.to_string()).or_default();
            match event {
                AlertEvent::Raised
                | AlertEvent::Flapping
                | AlertEvent::Settled { active: true } => {
                    entry.active.insert(alert.to_string());
                }
                AlertEvent::Cleared | AlertEvent::Settled { active: false } => {
                    entry.active.remove(alert);
                }
            }
        }
        let record = AlertRecord {
            at_ms,
            target: target.to_string(),
            alert: alert.to_string(),
            event: name.to_string(),
            comment: self
                .config
                .alerts
                .get(alert)
                .and_then(|a| a.comment.clone()),
        };
        if let Some(log) = self.log.lock().unwrap().as_mut()
            && let Ok(line) = serde_json::to_string(&record)
        {
            let _ = writeln!(log, "{line}");
        }
        self.history.lock().unwrap().push(record.clone());
        let event = serde_json::json!({
            "kind": "alert",
            "target": target,
            "status": self.status(target),
            "record": record,
        });
        let _ = self.events.send(event.to_string());
    }

    pub fn status(&self, target: &str) -> &'static str {
        let live = self.live.read().unwrap();
        let Some(entry) = live.get(target) else {
            return "unknown";
        };
        match &entry.last {
            None => "unknown",
            Some(last) if last.sent > 0 && last.received == 0 => "down",
            Some(_) if !entry.active.is_empty() => "alert",
            Some(last) if last.loss > 0.0 => "loss",
            Some(_) => "ok",
        }
    }

    pub fn last(&self, target: &str) -> Option<LastStep> {
        self.live
            .read()
            .unwrap()
            .get(target)
            .and_then(|l| l.last.clone())
    }

    pub fn active(&self, target: &str) -> Vec<String> {
        self.live
            .read()
            .unwrap()
            .get(target)
            .map(|l| l.active.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn history(&self) -> Vec<AlertRecord> {
        self.history.lock().unwrap().clone()
    }

    pub fn metrics(&self) -> String {
        let live = self.live.read().unwrap();
        let label = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut out = String::new();
        out.push_str("# HELP haze_pings_sent Probes sent since haze started.\n# TYPE haze_pings_sent counter\n");
        for (t, l) in live.iter() {
            out.push_str(&format!(
                "haze_pings_sent_total{{target=\"{}\"}} {}\n",
                label(t),
                l.sent
            ));
        }
        out.push_str("# HELP haze_pings_received Replies received since haze started.\n# TYPE haze_pings_received counter\n");
        for (t, l) in live.iter() {
            out.push_str(&format!(
                "haze_pings_received_total{{target=\"{}\"}} {}\n",
                label(t),
                l.received
            ));
        }
        out.push_str("# HELP haze_rtt_seconds Round trip times since haze started.\n# TYPE haze_rtt_seconds histogram\n");
        for (t, l) in live.iter() {
            let t = label(t);
            for (le, count) in METRIC_BUCKETS.iter().zip(l.buckets) {
                out.push_str(&format!(
                    "haze_rtt_seconds_bucket{{target=\"{t}\",le=\"{le}\"}} {count}\n"
                ));
            }
            out.push_str(&format!(
                "haze_rtt_seconds_bucket{{target=\"{t}\",le=\"+Inf\"}} {}\n",
                l.received
            ));
            out.push_str(&format!(
                "haze_rtt_seconds_sum{{target=\"{t}\"}} {}\n",
                l.sum_seconds
            ));
            out.push_str(&format!(
                "haze_rtt_seconds_count{{target=\"{t}\"}} {}\n",
                l.received
            ));
        }
        out.push_str("# HELP haze_step_loss_ratio Loss in the last completed step.\n# TYPE haze_step_loss_ratio gauge\n");
        for (t, l) in live.iter() {
            if let Some(last) = &l.last {
                out.push_str(&format!(
                    "haze_step_loss_ratio{{target=\"{}\"}} {}\n",
                    label(t),
                    last.loss
                ));
            }
        }
        out.push_str("# HELP haze_step_rtt_seconds Round trip quantiles of the last completed step.\n# TYPE haze_step_rtt_seconds gauge\n");
        for (t, l) in live.iter() {
            let Some(last) = &l.last else { continue };
            for (q, v) in [
                ("0", last.min_ms),
                ("0.5", last.median_ms),
                ("0.9", last.p90_ms),
                ("1", last.max_ms),
            ] {
                if let Some(v) = v {
                    out.push_str(&format!(
                        "haze_step_rtt_seconds{{target=\"{}\",quantile=\"{q}\"}} {}\n",
                        label(t),
                        v / 1e3
                    ));
                }
            }
        }
        out.push_str("# HELP haze_alert_active Alerts that are raised now.\n# TYPE haze_alert_active gauge\n");
        for (t, l) in live.iter() {
            for a in &l.active {
                out.push_str(&format!(
                    "haze_alert_active{{target=\"{}\",alert=\"{}\"}} 1\n",
                    label(t),
                    label(a)
                ));
            }
        }
        out.push_str("# EOF\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(at_ms: i64, target: &str, event: &str) -> AlertRecord {
        AlertRecord {
            at_ms,
            target: target.into(),
            alert: "loss".into(),
            event: event.into(),
            comment: None,
        }
    }

    #[test]
    fn incidents_pair_raises_with_clears_newest_first() {
        let list = incidents(&[
            record(1, "a", "raised"),
            record(2, "b", "raised"),
            record(3, "a", "cleared"),
            record(4, "b", "flapping"),
            record(5, "a", "raised"),
        ]);
        assert_eq!(list.len(), 3);
        assert_eq!(
            (
                list[0].target.as_str(),
                list[0].started_ms,
                list[0].ended_ms
            ),
            ("a", 5, None)
        );
        assert_eq!(
            (list[1].target.as_str(), list[1].ended_ms, list[1].flapping),
            ("b", None, true)
        );
        assert_eq!((list[2].started_ms, list[2].ended_ms), (1, Some(3)));
    }

    #[test]
    fn the_last_stored_step_is_known_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "[storage]\ndir = \"{}\"\n[targets.a]\nhost = \"192.0.2.1\"\n[targets.b]\nhost = \"192.0.2.2\"\n[targets.old]\nhost = \"192.0.2.3\"",
            dir.path().display()
        );
        let config = Config::parse(&text).unwrap();
        let now = 1_790_000_000_000;
        let step = |start_ms, lost: bool| Step {
            start_ms,
            step_ms: 60_000,
            rtts: vec![(!lost).then(|| Duration::from_millis(5)); 4],
            duplicates: 0,
            reordered: 0,
        };
        let mut store = haze_store::Store::open(dir.path()).unwrap();
        store.append("a", &step(now - 180_000, true)).unwrap();
        store.append("a", &step(now - 120_000, false)).unwrap();
        store.append("b", &step(now - 120_000, true)).unwrap();
        store
            .append("old", &step(now - 3 * 86_400_000, false))
            .unwrap();
        let hub = Hub::new(config, dir.path(), now).unwrap();
        assert_eq!(hub.status("a"), "ok");
        assert_eq!(hub.last("a").unwrap().at_ms, now - 120_000);
        assert_eq!(hub.status("b"), "down");
        assert_eq!(hub.status("old"), "unknown");
    }

    #[test]
    fn open_incidents_are_closed_when_haze_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::parse("[targets.a]\nhost = \"192.0.2.1\"").unwrap();
        let hub = Hub::new(config.clone(), dir.path(), 0).unwrap();
        hub.alert("a", "loss", AlertEvent::Raised, 10);
        drop(hub);
        let hub = Hub::new(config, dir.path(), 99).unwrap();
        let list = incidents(&hub.history());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].ended_ms, Some(99));
        assert_eq!(list[0].end_reason.as_deref(), Some("haze restarted"));
    }
}
