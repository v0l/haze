use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use haze_core::backoff::Backoff;
use haze_core::duration::format_ms;
use haze_core::{AlertEvent, AlertState, Config, Step, Summary, Target, smoke_line};
use haze_probe::{Sampler, StepDetail, build};
use haze_store::Store;
use haze_web::Hub;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::Instant;

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
    backing_off: Option<BackingOff>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackingOff {
    pub reason: String,
    pub next_in: Duration,
}

pub async fn sample_loop<F, Fut>(
    target: &Target,
    sampler: &Sampler,
    deadline: Option<Instant>,
    mut done: F,
) where
    F: FnMut(Step, StepDetail, Option<BackingOff>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut backoff = Backoff::new(target.step);
    let mut next = Instant::now();
    let mut last_start = next;
    let mut running = FuturesUnordered::new();
    loop {
        if deadline.is_some_and(|d| next >= d) && running.is_empty() {
            break;
        }
        let starting = !deadline.is_some_and(|d| next >= d);
        tokio::select! {
            _ = tokio::time::sleep_until(next), if starting => {
                last_start = Instant::now();
                running.push(Box::pin(sampler.step(target.pings, target.step, target.timeout)));
                next = last_start + backoff.interval();
            }
            Some((step, detail)) = running.next() => {
                let pushback = detail
                    .pushback
                    .as_ref()
                    .map(|p| (p.reason.as_str(), p.retry_after));
                let wait = backoff.observe(pushback);
                next = if pushback.is_some() {
                    next.max(last_start + wait)
                } else {
                    last_start + wait
                };
                let backing_off = backoff.reason().map(|reason| BackingOff {
                    reason: reason.to_string(),
                    next_in: next.saturating_duration_since(Instant::now()),
                });
                done(step, detail, backing_off).await;
            }
        }
    }
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
            let sampler = Sampler::new(probe);
            sample_loop(&target, &sampler, deadline, |step, detail, backing_off| {
                let tx = tx.clone();
                async move {
                    let _ = tx
                        .send(Measured {
                            target: index,
                            step,
                            detail,
                            backing_off,
                        })
                        .await;
                }
            })
            .await;
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
        hub.backing_off(
            &target.path,
            measured
                .backing_off
                .as_ref()
                .map(|b| haze_web::hub::BackingOff {
                    reason: b.reason.clone(),
                    next_in_ms: b.next_in.as_millis() as u64,
                }),
        );
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
            if let Some(b) = &measured.backing_off {
                line.push_str(&format!(
                    " [backing off: {}, next step in {}s]",
                    b.reason,
                    b.next_in.as_secs()
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

    struct Scripted {
        answers: std::sync::Mutex<VecDeque<Result<(), haze_core::ProbeError>>>,
    }

    impl haze_core::Probe for Scripted {
        fn probe(&self, _seq: u16, _timeout: Duration) -> haze_core::ProbeFuture<'_> {
            let answer = self.answers.lock().unwrap().pop_front().unwrap_or(Ok(()));
            Box::pin(async move { answer.map(|_| haze_core::Reply::rtt(Duration::from_millis(1))) })
        }
    }

    async fn run_script(
        step_ms: u64,
        answers: Vec<Result<(), haze_core::ProbeError>>,
        steps: usize,
    ) -> Vec<(Duration, Option<BackingOff>)> {
        let mut target = Config::parse("[targets.web]\nurl = \"http://127.0.0.1:9/\"")
            .unwrap()
            .targets
            .remove(0);
        target.step = Duration::from_millis(step_ms);
        target.timeout = Duration::from_millis(step_ms / 2);
        let sampler = Sampler::new(Arc::new(Scripted {
            answers: std::sync::Mutex::new(answers.into()),
        }));
        let started = Instant::now();
        let seen = std::sync::Mutex::new(Vec::new());
        let deadline = Some(started + Duration::from_millis(step_ms * 40));
        let run = sample_loop(&target, &sampler, deadline, |_, _, backing_off| {
            seen.lock().unwrap().push((started.elapsed(), backing_off));
            async {}
        });
        let watch = async {
            while seen.lock().unwrap().len() < steps {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::select! {
            _ = run => {}
            _ = watch => {}
        }
        seen.into_inner().unwrap()
    }

    fn refused(status: u16, retry_after: Option<u64>) -> Result<(), haze_core::ProbeError> {
        Err(haze_core::ProbeError::Refused {
            status,
            retry_after: retry_after.map(Duration::from_secs),
        })
    }

    #[tokio::test]
    async fn refusals_double_the_step_and_three_clean_steps_reset_it() {
        let seen = run_script(100, vec![refused(429, None), refused(429, None)], 7).await;
        let at: Vec<u128> = seen.iter().map(|(t, _)| t.as_millis()).collect();
        let gaps: Vec<u128> = at.windows(2).map(|w| w[1] - w[0]).collect();
        let expect = [200, 400, 400, 400, 100, 100];
        for (gap, want) in gaps.iter().zip(expect) {
            assert!(
                *gap + 20 >= want && *gap < want + 150,
                "gaps {gaps:?}, want {expect:?}"
            );
        }
        assert_eq!(seen[0].1.as_ref().unwrap().reason, "HTTP 429");
        assert!(
            seen[3].1.is_some(),
            "still backing off after two clean steps"
        );
        assert_eq!(seen[4].1, None, "reset after three clean steps");
    }

    #[tokio::test]
    async fn retry_after_sets_the_next_step() {
        let seen = run_script(100, vec![refused(503, Some(1))], 2).await;
        let gap = seen[1].0 - seen[0].0;
        assert!(
            gap >= Duration::from_millis(980) && gap < Duration::from_millis(1300),
            "{gap:?}"
        );
        let first = seen[0].1.as_ref().unwrap();
        assert_eq!(first.reason, "HTTP 503");
        assert!(first.next_in > Duration::from_millis(800), "{first:?}");
    }

    #[test]
    fn clock_is_utc_time_of_day() {
        assert_eq!(clock(0), "00:00:00");
        assert_eq!(clock(1_700_000_000_000), "22:13:20");
    }
}
