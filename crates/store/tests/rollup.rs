use std::time::Duration;

use haze_core::Step;
use haze_core::histogram::{RELATIVE_ERROR, exact_quantile};
use haze_store::{Store, date_name};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const START: i64 = 20_400 * DAY;

struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn rtt(&mut self) -> Option<Duration> {
        if self.unit() < 0.02 {
            return None;
        }
        let ms = 3.0 + 40.0 * self.unit().powi(4) + self.unit();
        Some(Duration::from_micros((ms * 1e3) as u64))
    }
}

fn fill(store: &mut Store, target: &str, days: i64, step_ms: i64, seed: u64) -> Vec<Step> {
    let mut rng = Lcg(seed);
    let mut steps = Vec::new();
    let mut at = START;
    while at < START + days * DAY {
        let step = Step {
            start_ms: at,
            step_ms: step_ms as u32,
            rtts: (0..20).map(|_| rng.rtt()).collect(),
            duplicates: 0,
            reordered: 0,
            refused: 0,
        };
        store.append(target, &step).unwrap();
        steps.push(step);
        at += step_ms;
    }
    steps
}

fn within(estimate: Duration, exact: Duration) -> bool {
    let (e, x) = (estimate.as_nanos() as f64, exact.as_nanos() as f64);
    (e - x).abs() <= x * RELATIVE_ERROR + 1.0
}

#[test]
fn a_seven_day_query_over_rollups_keeps_percentiles_within_the_bucket_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let steps = fill(&mut store, "t", 8, 60_000, 11);
    let written = store.rollup_closed_days(START + 7 * DAY + HOUR).unwrap();
    assert_eq!(written, 7);
    for day in 0..7 {
        let name = format!("{}.hzr", date_name(START / DAY + day));
        assert!(dir.path().join("t").join(name).exists());
    }
    std::fs::write(
        dir.path()
            .join("t")
            .join(format!("{}.haze", date_name(START / DAY + 3))),
        b"",
    )
    .unwrap();

    let store = Store::open(dir.path()).unwrap();
    let buckets = store.query("t", START, START + 7 * DAY, 7 * DAY).unwrap();
    assert_eq!(buckets.len(), 1);
    let b = &buckets[0];
    let seven: Vec<&Step> = steps
        .iter()
        .filter(|s| s.start_ms < START + 7 * DAY)
        .collect();
    assert_eq!(
        b.steps as usize,
        seven.len(),
        "day 3 came from its rollup, not the emptied raw file"
    );
    let mut all: Vec<Duration> = seven
        .iter()
        .flat_map(|s| s.rtts.iter().flatten().copied())
        .collect();
    all.sort();
    assert_eq!(b.received as usize, all.len());
    assert_eq!(b.sent as usize, seven.len() * 20);
    for q in [0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99] {
        let estimate = b.histogram.quantile(q).unwrap();
        let exact = exact_quantile(&all, q).unwrap();
        assert!(within(estimate, exact), "q {q}: {estimate:?} vs {exact:?}");
    }
    assert_eq!(b.histogram.min(), all.first().copied());
    assert_eq!(b.histogram.max(), all.last().copied());
}

#[test]
fn hourly_and_daily_rollups_match_raw_queries() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    fill(&mut store, "t", 3, 300_000, 5);
    let raw_hours = store.query("t", START, START + 3 * DAY, HOUR).unwrap();
    let raw_days = store.query("t", START, START + 3 * DAY, DAY).unwrap();
    let odd_width = store
        .query("t", START, START + 3 * DAY, 7 * 60_000)
        .unwrap();
    store.rollup_closed_days(START + 3 * DAY).unwrap();
    assert_eq!(
        store.query("t", START, START + 3 * DAY, HOUR).unwrap(),
        raw_hours
    );
    assert_eq!(
        store.query("t", START, START + 3 * DAY, DAY).unwrap(),
        raw_days
    );
    assert_eq!(
        store
            .query("t", START, START + 3 * DAY, 7 * 60_000)
            .unwrap(),
        odd_width
    );
    let partial = store
        .query("t", START + 90 * 60_000, START + DAY + 30 * 60_000, HOUR)
        .unwrap();
    let raw_partial: Vec<_> = {
        let fresh = Store::open(dir.path()).unwrap();
        for day in 0..3 {
            let _ = std::fs::remove_file(
                dir.path()
                    .join("t")
                    .join(format!("{}.hzr", date_name(START / DAY + day))),
            );
        }
        fresh
            .query("t", START + 90 * 60_000, START + DAY + 30 * 60_000, HOUR)
            .unwrap()
    };
    assert_eq!(partial, raw_partial);
}

#[test]
fn appending_to_a_rolled_up_day_drops_its_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    fill(&mut store, "t", 1, 600_000, 9);
    store.rollup_closed_days(START + DAY).unwrap();
    let late = Step {
        start_ms: START + DAY - 1000,
        step_ms: 60_000,
        rtts: vec![Some(Duration::from_millis(500))],
        duplicates: 0,
        reordered: 0,
        refused: 0,
    };
    let mut store = Store::open(dir.path()).unwrap();
    store.append("t", &late).unwrap();
    let day = store.query("t", START, START + DAY, DAY).unwrap();
    assert_eq!(day[0].histogram.max(), Some(Duration::from_millis(500)));
}

#[test]
fn a_corrupt_rollup_falls_back_to_raw_steps() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    fill(&mut store, "t", 1, 600_000, 4);
    let raw = store.query("t", START, START + DAY, DAY).unwrap();
    store.rollup_closed_days(START + DAY).unwrap();
    let path = dir
        .path()
        .join("t")
        .join(format!("{}.hzr", date_name(START / DAY)));
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(store.query("t", START, START + DAY, DAY).unwrap(), raw);
    assert_eq!(store.targets().unwrap(), ["t"]);
}
