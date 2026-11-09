use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haze_core::{Config, Probe, ProbeFuture, Reply};
use haze_probe::limit::{Limited, Limiter, limiters};

struct Stamp(Arc<Mutex<Vec<Instant>>>);

impl Probe for Stamp {
    fn probe(&self, _seq: u16, _timeout: Duration) -> ProbeFuture<'_> {
        Box::pin(async move {
            self.0.lock().unwrap().push(Instant::now());
            Ok(Reply::rtt(Duration::from_millis(1)))
        })
    }
}

#[tokio::test]
async fn targets_on_one_destination_share_one_budget() {
    let starts: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let limiter = Arc::new(Limiter::new(Duration::from_millis(100)));
    let a = Arc::new(Limited::new(
        Arc::new(Stamp(starts.clone())),
        limiter.clone(),
    ));
    let b = Arc::new(Limited::new(
        Arc::new(Stamp(starts.clone())),
        limiter.clone(),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for seq in 0..3 {
        let (a, b) = (a.clone(), b.clone());
        tasks.spawn(async move { a.probe(seq, Duration::from_secs(1)).await });
        tasks.spawn(async move { b.probe(seq, Duration::from_secs(1)).await });
    }
    while let Some(done) = tasks.join_next().await {
        done.unwrap().unwrap();
    }
    let mut starts = starts.lock().unwrap().clone();
    starts.sort();
    assert_eq!(starts.len(), 6);
    for pair in starts.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap >= Duration::from_millis(95), "gap {gap:?}");
    }
}

#[test]
fn limiters_are_keyed_by_host_and_port() {
    let config = Config::parse(
        r#"
[targets.a]
url = "https://example.com/a"
[targets.b]
url = "https://Example.com:443/b"
[targets.c]
url = "http://example.com/"
[targets.d]
host = "example.com"
probe = "tcp"
port = 443
[targets.ping]
host = "example.com"
[targets.fast]
url = "https://fast.example/"
pings = 20
allow_aggressive = true
"#,
    )
    .unwrap();
    let map = limiters(&config.targets);
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["example.com:443", "example.com:80", "fast.example:443"]
    );
    assert_eq!(map["example.com:443"].spacing(), Duration::from_secs(10));
    assert_eq!(map["fast.example:443"].spacing(), Duration::from_secs(3));
}
