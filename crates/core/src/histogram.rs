use std::collections::BTreeMap;
use std::time::Duration;

pub const GROWTH: f64 = 1.02;
pub const RELATIVE_ERROR: f64 = (GROWTH - 1.0) / (GROWTH + 1.0);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Histogram {
    buckets: BTreeMap<i32, u64>,
    count: u64,
    sum_us: f64,
    min_us: f64,
    max_us: f64,
}

fn bucket_of(us: f64) -> i32 {
    if us < 1.0 {
        return 0;
    }
    (us.ln() / GROWTH.ln()).floor() as i32
}

fn representative(index: i32) -> f64 {
    let low = GROWTH.powi(index);
    2.0 * low * GROWTH / (1.0 + GROWTH)
}

impl Histogram {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, value: Duration) {
        let us = value.as_nanos() as f64 / 1e3;
        *self.buckets.entry(bucket_of(us)).or_insert(0) += 1;
        if self.count == 0 {
            self.min_us = us;
            self.max_us = us;
        } else {
            self.min_us = self.min_us.min(us);
            self.max_us = self.max_us.max(us);
        }
        self.count += 1;
        self.sum_us += us;
    }

    pub fn merge(&mut self, other: &Histogram) {
        if other.count == 0 {
            return;
        }
        for (index, count) in &other.buckets {
            *self.buckets.entry(*index).or_insert(0) += count;
        }
        if self.count == 0 {
            self.min_us = other.min_us;
            self.max_us = other.max_us;
        } else {
            self.min_us = self.min_us.min(other.min_us);
            self.max_us = self.max_us.max(other.max_us);
        }
        self.count += other.count;
        self.sum_us += other.sum_us;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    pub fn min(&self) -> Option<Duration> {
        (self.count > 0).then(|| from_us(self.min_us))
    }

    pub fn max(&self) -> Option<Duration> {
        (self.count > 0).then(|| from_us(self.max_us))
    }

    pub fn mean(&self) -> Option<Duration> {
        (self.count > 0).then(|| from_us(self.sum_us / self.count as f64))
    }

    pub fn quantile(&self, q: f64) -> Option<Duration> {
        if self.count == 0 {
            return None;
        }
        let rank = ((q.clamp(0.0, 1.0) * self.count as f64).ceil() as u64).clamp(1, self.count);
        if rank == 1 {
            return self.min();
        }
        if rank == self.count {
            return self.max();
        }
        let mut seen = 0;
        for (index, count) in &self.buckets {
            seen += count;
            if seen >= rank {
                let estimate = representative(*index).clamp(self.min_us, self.max_us);
                return Some(from_us(estimate));
            }
        }
        self.max()
    }

    pub fn buckets(&self) -> impl Iterator<Item = (Duration, Duration, u64)> + '_ {
        self.buckets.iter().map(|(index, count)| {
            (
                from_us(GROWTH.powi(*index)),
                from_us(GROWTH.powi(index + 1)),
                *count,
            )
        })
    }
}

fn from_us(us: f64) -> Duration {
    Duration::from_nanos((us * 1e3).round().max(0.0) as u64)
}

pub fn exact_quantile(sorted: &[Duration], q: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = ((q.clamp(0.0, 1.0) * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted[rank - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next_f64(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        fn rtt(&mut self) -> Duration {
            let ms = 0.05 * (2000.0f64).powf(self.next_f64());
            Duration::from_nanos((ms * 1e6) as u64)
        }
    }

    fn within_bound(estimate: Duration, exact: Duration) -> bool {
        let e = estimate.as_nanos() as f64;
        let x = exact.as_nanos() as f64;
        (e - x).abs() <= x * RELATIVE_ERROR + 1.0
    }

    const QUANTILES: [f64; 9] = [0.0, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 1.0];

    #[test]
    fn quantiles_are_within_the_bucket_error_of_exact_values() {
        let mut rng = Lcg(7);
        for size in [1usize, 2, 3, 20, 101, 5000] {
            let values: Vec<Duration> = (0..size).map(|_| rng.rtt()).collect();
            let mut histogram = Histogram::new();
            values.iter().for_each(|v| histogram.record(*v));
            let mut sorted = values.clone();
            sorted.sort();
            for q in QUANTILES {
                let estimate = histogram.quantile(q).unwrap();
                let exact = exact_quantile(&sorted, q).unwrap();
                assert!(
                    within_bound(estimate, exact),
                    "size {size} q {q}: {estimate:?} vs {exact:?}"
                );
            }
        }
    }

    #[test]
    fn merged_histograms_match_one_built_from_all_values() {
        let mut rng = Lcg(42);
        let mut whole = Histogram::new();
        let mut merged = Histogram::new();
        let mut all = Vec::new();
        for _ in 0..288 {
            let mut step = Histogram::new();
            for _ in 0..20 {
                let v = rng.rtt();
                step.record(v);
                whole.record(v);
                all.push(v);
            }
            merged.merge(&step);
        }
        assert_eq!(merged.buckets, whole.buckets);
        assert_eq!(merged.count(), whole.count());
        assert_eq!(merged.min(), whole.min());
        assert_eq!(merged.max(), whole.max());
        all.sort();
        for q in QUANTILES {
            let estimate = merged.quantile(q).unwrap();
            let exact = exact_quantile(&all, q).unwrap();
            assert!(
                within_bound(estimate, exact),
                "q {q}: {estimate:?} vs {exact:?}"
            );
        }
    }

    #[test]
    fn empty_histogram_has_no_quantiles() {
        let mut histogram = Histogram::new();
        assert_eq!(histogram.quantile(0.5), None);
        histogram.merge(&Histogram::new());
        assert_eq!(histogram.count(), 0);
        assert_eq!(histogram.min(), None);
    }

    #[test]
    fn min_and_max_are_exact() {
        let mut histogram = Histogram::new();
        for us in [1234, 98765, 5555] {
            histogram.record(Duration::from_micros(us));
        }
        assert_eq!(histogram.min(), Some(Duration::from_micros(1234)));
        assert_eq!(histogram.max(), Some(Duration::from_micros(98765)));
        assert_eq!(histogram.quantile(0.0), histogram.min());
        assert_eq!(histogram.quantile(1.0), histogram.max());
    }

    #[test]
    fn sub_microsecond_values_share_the_first_bucket() {
        let mut histogram = Histogram::new();
        histogram.record(Duration::from_nanos(300));
        histogram.record(Duration::from_nanos(900));
        assert_eq!(histogram.bucket_count(), 1);
    }
}
