use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use haze_core::duration::format_ms;
use haze_core::{AlertEvent, AlertState, Config, Step, Summary, smoke_line};
use haze_probe::{Sampler, StepDetail, build};
use haze_store::Store;
use haze_web::Hub;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

use crate::scale_for;

const HISTORY: usize = 1024;
const RETRY: Duration = Duration::from_secs(30);

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

struct Measured {
    target: usize,
    step: Step,
    detail: StepDetail,
}

pub struct Evaluator {
    config: Config,
    history: Vec<VecDeque<Summary>>,
    states: HashMap<(usize, String), AlertState>,
}

#[derive(Debug, PartialEq)]
pub struct Notice {
    pub target: String,
    pub alert: String,
    pub event: AlertEvent,
}

impl Evaluator {
    pub fn new(config: Config) -> Evaluator {
        let history = vec![VecDeque::new(); config.targets.len()];
        Evaluator {
            config,
            history,
            states: HashMap::new(),
        }
    }

    fn is_down(&self, path: &str) -> bool {
        let Some(index) = self.config.targets.iter().position(|t| t.path == path) else {
            return false;
        };
        self.states
            .iter()
            .any(|((t, _), s)| *t == index && s.active)
    }

    pub fn observe(&mut self, target: usize, summary: Summary) -> Vec<Notice> {
        let history = &mut self.history[target];
        history.push_back(summary);
        while history.len() > HISTORY {
            history.pop_front();
        }
        let series: Vec<Summary> = history.iter().cloned().collect();
        let config = &self.config.targets[target];
        let held = haze_core::matcher::held_by_dependency(
            config.depends.iter().map(String::as_str),
            |p| self.is_down(p),
        );
        let mut notices = Vec::new();
        for name in config.alerts.clone() {
            let alert = &self.config.alerts[&name];
            let matched = !held && alert.matcher.matches(&series);
            let state = self.states.entry((target, name.clone())).or_default();
            if let Some(event) = state.update(matched, &alert.policy) {
                notices.push(Notice {
                    target: self.config.targets[target].path.clone(),
                    alert: name,
                    event,
                });
            }
        }
        notices
    }
}

pub struct Options {
    pub duration: Option<Duration>,
    pub quiet: bool,
    pub listen: Option<SocketAddr>,
    pub notify: Vec<crate::notify::Route>,
}

pub async fn run(config: Config, options: Options) -> Result<()> {
    let Options {
        duration,
        quiet,
        listen,
        notify,
    } = options;
    let mut store = Store::open(&config.storage_dir)?;
    let hub = Arc::new(Hub::new(config.clone(), &config.storage_dir, now_ms())?);
    if let Some(listen) = listen {
        let app = haze_web::router(hub.clone())?;
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("listening on {listen}"))?;
        eprintln!("dashboard on http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("the dashboard stopped: {e}");
            }
        });
    }
    let notifier = crate::notify::Notifier::new(notify);
    let started = Instant::now();
    let deadline = duration.map(|d| started + d);
    let (tx, mut rx) = mpsc::channel::<Measured>(256);
    let width = config
        .targets
        .iter()
        .map(|t| t.path.len())
        .max()
        .unwrap_or(0);
    let mut workers = Vec::new();
    let limiters = haze_probe::limit::limiters(&config.targets);
    for (index, target) in config.targets.iter().cloned().enumerate() {
        let tx = tx.clone();
        let limiter = limiters.get(&target.destination()).cloned();
        workers.push(tokio::spawn(async move {
            let probe = loop {
                match build(&target).await {
                    Ok(p) => break p,
                    Err(e) => {
                        eprintln!("{}: {e}, retrying in {}s", target.path, RETRY.as_secs());
                        if deadline.is_some_and(|d| Instant::now() + RETRY > d) {
                            return;
                        }
                        tokio::time::sleep(RETRY).await;
                    }
                }
            };
            let probe: Arc<dyn haze_core::Probe> = match limiter {
                Some(limiter) => Arc::new(haze_probe::limit::Limited::new(probe, limiter)),
                None => probe,
            };
            let sampler = Arc::new(Sampler::new(probe));
            let mut ticker = tokio::time::interval(target.step);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut running = tokio::task::JoinSet::new();
            loop {
                let tick = ticker.tick().await;
                if deadline.is_some_and(|d| tick >= d) {
                    break;
                }
                let sampler = sampler.clone();
                let tx = tx.clone();
                let (pings, step, timeout) = (target.pings, target.step, target.timeout);
                running.spawn(async move {
                    let (step, detail) = sampler.step(pings, step, timeout).await;
                    let _ = tx
                        .send(Measured {
                            target: index,
                            step,
                            detail,
                        })
                        .await;
                });
            }
            running.join_all().await;
        }));
    }
    drop(tx);
    let mut evaluator = Evaluator::new(config.clone());
    let retention_ms = config.retention_days as i64 * 86_400_000;
    let mut last_prune = 0;
    loop {
        let measured = tokio::select! {
            m = rx.recv() => match m { Some(m) => m, None => break },
            _ = tokio::signal::ctrl_c() => break,
        };
        let target = &config.targets[measured.target];
        if let Err(e) = store.append(&target.path, &measured.step) {
            eprintln!("{}: storing the step failed: {e}", target.path);
        }
        hub.step_done(&target.path, &measured.step);
        let summary = measured.step.summary();
        if !quiet {
            let mut line = format!(
                "{} {:<width$} {}",
                clock(measured.step.start_ms),
                target.path,
                smoke_line(&measured.step, scale_for(summary.max)),
            );
            if let Some(reply) = &measured.detail.last_reply
                && !reply.phases.is_empty()
            {
                let phases: Vec<String> = reply
                    .phases
                    .iter()
                    .map(|(n, d)| format!("{n} {}", format_ms(*d)))
                    .collect();
                line.push_str(&format!(" ({})", phases.join(" ")));
            }
            if let Some((_, error)) = measured.detail.errors.first() {
                line.push_str(&format!(
                    " [{} errors: {error}]",
                    measured.detail.errors.len()
                ));
            }
            println!("{line}");
        }
        for notice in evaluator.observe(measured.target, summary.clone()) {
            let at = now_ms();
            println!(
                "{} ALERT {} {}: {:?}",
                clock(at),
                notice.target,
                notice.alert,
                notice.event
            );
            hub.alert(&notice.target, &notice.alert, notice.event, at);
            notifier.send_tagged(
                crate::notify::Message {
                    target: notice.target.clone(),
                    alert: notice.alert.clone(),
                    event: haze_web::hub::event_name(notice.event).to_string(),
                    at_ms: at,
                    comment: config
                        .alerts
                        .get(&notice.alert)
                        .and_then(|a| a.comment.clone()),
                    loss: summary.loss,
                    median_ms: haze_web::hub::ms(summary.median),
                },
                target.tags.clone(),
            );
        }
        if now_ms() - last_prune > 3_600_000 {
            last_prune = now_ms();
            if let Err(e) = store.prune(now_ms() - retention_ms) {
                eprintln!("pruning old data failed: {e}");
            }
            if let Err(e) = store.rollup_closed_days(now_ms()) {
                eprintln!("rolling up old days failed: {e}");
            }
        }
    }
    store.sync()?;
    notifier.finish().await;
    for worker in workers {
        worker.abort();
    }
    Ok(())
}

fn clock(ms: i64) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
[alerts.down]
when = "loss >= 50% for 2 steps"

[targets.router]
host = "192.0.2.1"
alerts = ["down"]

[targets.behind]
host = "192.0.2.2"
alerts = ["down"]
depends = ["router"]
"#;

    fn summary(loss: f64) -> Summary {
        let rtts: Vec<Option<Duration>> = (0..10)
            .map(|i| ((i as f64) >= loss * 10.0).then(|| Duration::from_millis(5)))
            .collect();
        Step {
            start_ms: 0,
            step_ms: 60_000,
            rtts,
            duplicates: 0,
            reordered: 0,
            refused: 0,
        }
        .summary()
    }

    #[test]
    fn a_child_alert_is_held_while_its_parent_is_down() {
        let config = Config::parse(CONFIG).unwrap();
        let index = |p: &str| config.targets.iter().position(|t| t.path == p).unwrap();
        let (router, behind) = (index("router"), index("behind"));
        let mut e = Evaluator::new(config);
        assert!(e.observe(router, summary(1.0)).is_empty());
        assert!(e.observe(behind, summary(1.0)).is_empty());
        let raised = e.observe(router, summary(1.0));
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].event, AlertEvent::Raised);
        assert!(e.observe(behind, summary(1.0)).is_empty(), "held by router");
        let cleared = e.observe(router, summary(0.0));
        assert_eq!(cleared[0].event, AlertEvent::Cleared);
        let child = e.observe(behind, summary(1.0));
        assert_eq!(
            child,
            vec![Notice {
                target: "behind".into(),
                alert: "down".into(),
                event: AlertEvent::Raised
            }]
        );
    }

    #[test]
    fn clock_is_utc_time_of_day() {
        assert_eq!(clock(0), "00:00:00");
        assert_eq!(clock(1_700_000_000_000), "22:13:20");
    }
}
