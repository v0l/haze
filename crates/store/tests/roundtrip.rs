use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use pingwatch_core::{Histogram, Step};
use pingwatch_store::{Store, date_name, day_of};

const DAY: i64 = 86_400_000;
const T0: i64 = 1_790_000_000_000;

fn step(start_ms: i64, base_us: u64) -> Step {
    let mut rtts: Vec<Option<Duration>> = (0..20)
        .map(|i| Some(Duration::from_micros(base_us + i * 37)))
        .collect();
    rtts[3] = None;
    Step {
        start_ms,
        step_ms: 60_000,
        rtts,
        duplicates: 1,
        reordered: 2,
    }
}

#[test]
fn steps_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let written: Vec<Step> = (0..50)
        .map(|i| step(T0 + i * 60_000, 10_000 + i as u64))
        .collect();
    {
        let mut store = Store::open(dir.path()).unwrap();
        for s in &written {
            store.append("dns/cloudflare", s).unwrap();
        }
        store.sync().unwrap();
    }
    let store = Store::open(dir.path()).unwrap();
    let read = store.steps("dns/cloudflare", T0, T0 + DAY).unwrap();
    assert_eq!(read, written);
    assert_eq!(store.targets().unwrap(), ["dns/cloudflare"]);
}

#[test]
fn appending_after_a_restart_continues_the_day_file() {
    let dir = tempfile::tempdir().unwrap();
    Store::open(dir.path())
        .unwrap()
        .append("a", &step(T0, 5_000))
        .unwrap();
    Store::open(dir.path())
        .unwrap()
        .append("a", &step(T0 + 60_000, 6_000))
        .unwrap();
    let steps = Store::open(dir.path())
        .unwrap()
        .steps("a", T0, T0 + DAY)
        .unwrap();
    assert_eq!(steps.len(), 2);
}

#[test]
fn a_torn_last_record_is_dropped_and_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(dir.path()).unwrap();
        store.append("a", &step(T0, 5_000)).unwrap();
        store.append("a", &step(T0 + 60_000, 6_000)).unwrap();
    }
    let file = dir
        .path()
        .join("a")
        .join(format!("{}.pws", date_name(day_of(T0))));
    let full = std::fs::metadata(&file).unwrap().len();
    let f = OpenOptions::new().write(true).open(&file).unwrap();
    f.set_len(full - 7).unwrap();
    drop(f);
    let mut f = OpenOptions::new().append(true).open(&file).unwrap();
    f.write_all(b"garbage").unwrap();
    drop(f);

    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(store.steps("a", T0, T0 + DAY).unwrap().len(), 1);
    store.append("a", &step(T0 + 120_000, 7_000)).unwrap();
    let steps = Store::open(dir.path())
        .unwrap()
        .steps("a", T0, T0 + DAY)
        .unwrap();
    let starts: Vec<i64> = steps.iter().map(|s| s.start_ms).collect();
    assert_eq!(starts, [T0, T0 + 120_000]);
}

#[test]
fn steps_land_in_their_utc_day_and_queries_span_days() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let midnight = (T0 / DAY + 1) * DAY;
    let starts = [
        midnight - 120_000,
        midnight - 60_000,
        midnight,
        midnight + 60_000,
    ];
    for s in starts {
        store.append("x", &step(s, 1_000)).unwrap();
    }
    assert_eq!(store.days("x").unwrap().len(), 2);
    let all = store.steps("x", midnight - DAY, midnight + DAY).unwrap();
    assert_eq!(all.iter().map(|s| s.start_ms).collect::<Vec<_>>(), starts);
    let second_day = store.steps("x", midnight, midnight + DAY).unwrap();
    assert_eq!(second_day.len(), 2);
}

#[test]
fn query_buckets_merge_step_histograms() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let steps: Vec<Step> = (0..120)
        .map(|i| step(T0 + i * 60_000, 8_000 + (i as u64 % 7) * 500))
        .collect();
    for s in &steps {
        store.append("t", s).unwrap();
    }
    let hour = 3_600_000;
    let buckets = store.query("t", T0, T0 + 2 * hour, hour).unwrap();
    assert_eq!(buckets.len(), 2);
    for (b, chunk) in buckets.iter().zip(steps.chunks(60)) {
        let mut expected = Histogram::new();
        chunk.iter().for_each(|s| expected.merge(&s.histogram()));
        assert_eq!(b.histogram, expected);
        assert_eq!(b.steps, 60);
        assert_eq!(b.sent, 1200);
        assert_eq!(b.received, 1140);
        assert_eq!(b.duplicates, 60);
        assert!((b.loss() - 0.05).abs() < 1e-9);
    }
    let whole = store.query("t", T0, T0 + 2 * hour, 2 * hour).unwrap();
    let mut merged = buckets[0].histogram.clone();
    merged.merge(&buckets[1].histogram);
    assert_eq!(whole[0].histogram, merged);
    assert_eq!(whole[0].histogram.quantile(0.5), merged.quantile(0.5));
}

#[test]
fn prune_removes_whole_old_days() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    for d in 0..5 {
        store.append("p", &step(T0 + d * DAY, 1_000)).unwrap();
    }
    assert_eq!(store.prune(T0 + 3 * DAY).unwrap(), 3);
    assert_eq!(store.days("p").unwrap().len(), 2);
    store.append("p", &step(T0 + 5 * DAY, 1_000)).unwrap();
    assert_eq!(store.days("p").unwrap().len(), 3);
}

#[test]
fn rejects_target_paths_outside_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(store.append("../escape", &step(T0, 1)).is_err());
}
