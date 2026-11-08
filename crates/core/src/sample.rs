use std::time::Duration;

use crate::duration::format_ms;
use crate::histogram::{Histogram, exact_quantile};

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub start_ms: i64,
    pub step_ms: u32,
    pub rtts: Vec<Option<Duration>>,
    pub duplicates: u32,
    pub reordered: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub sent: u32,
    pub received: u32,
    pub loss: f64,
    pub min: Option<Duration>,
    pub p10: Option<Duration>,
    pub p25: Option<Duration>,
    pub median: Option<Duration>,
    pub p75: Option<Duration>,
    pub p90: Option<Duration>,
    pub p99: Option<Duration>,
    pub max: Option<Duration>,
    pub mean: Option<Duration>,
    pub jitter: Option<Duration>,
    pub pdv: Option<Duration>,
}

impl Summary {
    pub fn smoke(&self) -> Option<Duration> {
        Some(self.p90?.saturating_sub(self.p10?))
    }
}

impl Step {
    pub fn sent(&self) -> u32 {
        self.rtts.len() as u32
    }

    pub fn received(&self) -> u32 {
        self.rtts.iter().filter(|r| r.is_some()).count() as u32
    }

    pub fn histogram(&self) -> Histogram {
        let mut histogram = Histogram::new();
        self.rtts
            .iter()
            .flatten()
            .for_each(|r| histogram.record(*r));
        histogram
    }

    pub fn summary(&self) -> Summary {
        let mut sorted: Vec<Duration> = self.rtts.iter().flatten().copied().collect();
        sorted.sort();
        let sent = self.sent();
        let received = sorted.len() as u32;
        let q = |p| exact_quantile(&sorted, p);
        let mean = (!sorted.is_empty()).then(|| sorted.iter().sum::<Duration>() / received);
        let p99 = q(0.99);
        Summary {
            sent,
            received,
            loss: if sent == 0 {
                0.0
            } else {
                1.0 - received as f64 / sent as f64
            },
            min: sorted.first().copied(),
            p10: q(0.10),
            p25: q(0.25),
            median: q(0.50),
            p75: q(0.75),
            p90: q(0.90),
            p99,
            max: sorted.last().copied(),
            mean,
            jitter: rfc3550_jitter(&self.rtts),
            pdv: p99.zip(sorted.first().copied()).map(|(hi, lo)| hi - lo),
        }
    }
}

pub fn rfc3550_jitter(rtts: &[Option<Duration>]) -> Option<Duration> {
    let mut previous: Option<f64> = None;
    let mut jitter: Option<f64> = None;
    for rtt in rtts.iter().flatten() {
        let now = rtt.as_secs_f64();
        if let Some(before) = previous {
            let d = (now - before).abs();
            let j = jitter.unwrap_or(0.0);
            jitter = Some(j + (d - j) / 16.0);
        }
        previous = Some(now);
    }
    jitter.map(Duration::from_secs_f64)
}

pub fn count_reordered(arrivals: &[(u16, Duration)]) -> u32 {
    let mut sorted = arrivals.to_vec();
    sorted.sort_by_key(|(_, at)| *at);
    let mut highest: Option<u16> = None;
    let mut reordered = 0;
    for (seq, _) in sorted {
        match highest {
            Some(h) if seq < h => reordered += 1,
            _ => highest = Some(seq),
        }
    }
    reordered
}

const SMOKE_WIDTH: usize = 40;
const SMOKE_SHADES: [char; 5] = [' ', '░', '▒', '▓', '█'];

pub fn smoke_line(step: &Step, scale_max: Duration) -> String {
    let summary = step.summary();
    let mut cells = [0u32; SMOKE_WIDTH];
    let scale = scale_max.as_secs_f64().max(1e-6);
    let cell_of = |d: Duration| ((d.as_secs_f64() / scale) * SMOKE_WIDTH as f64).floor() as usize;
    for rtt in step.rtts.iter().flatten() {
        cells[cell_of(*rtt).min(SMOKE_WIDTH - 1)] += 1;
    }
    let peak = cells.iter().copied().max().unwrap_or(0).max(1);
    let mut line: Vec<char> = cells
        .iter()
        .map(|c| {
            if *c == 0 {
                SMOKE_SHADES[0]
            } else {
                SMOKE_SHADES[1 + ((*c as usize - 1) * 3 / peak as usize).min(3)]
            }
        })
        .collect();
    if let Some(median) = summary.median {
        line[cell_of(median).min(SMOKE_WIDTH - 1)] = '|';
    }
    let line: String = line.into_iter().collect();
    let ms = |d: Option<Duration>| d.map(format_ms).unwrap_or_else(|| "-".into());
    format!(
        "[{line}] loss {:>3.0}% min {} med {} p90 {} max {} jit {} ms",
        summary.loss * 100.0,
        ms(summary.min),
        ms(summary.median),
        ms(summary.p90),
        ms(summary.max),
        ms(summary.jitter),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Option<Duration> {
        Some(Duration::from_millis(v))
    }

    #[test]
    fn summary_counts_loss_and_exact_percentiles() {
        let mut rtts: Vec<Option<Duration>> = (1..=18).map(ms).collect();
        rtts.push(None);
        rtts.push(None);
        let step = Step {
            start_ms: 0,
            step_ms: 60_000,
            rtts,
            duplicates: 0,
            reordered: 0,
        };
        let s = step.summary();
        assert_eq!((s.sent, s.received), (20, 18));
        assert!((s.loss - 0.1).abs() < 1e-9);
        assert_eq!(s.min, ms(1));
        assert_eq!(s.median, ms(9));
        assert_eq!(s.p90, ms(17));
        assert_eq!(s.max, ms(18));
        assert_eq!(s.smoke(), Some(Duration::from_millis(15)));
    }

    #[test]
    fn total_loss_has_no_latency() {
        let step = Step {
            start_ms: 0,
            step_ms: 60_000,
            rtts: vec![None; 5],
            duplicates: 0,
            reordered: 0,
        };
        let s = step.summary();
        assert_eq!(s.loss, 1.0);
        assert_eq!(s.median, None);
        assert_eq!(s.jitter, None);
        assert!(smoke_line(&step, Duration::from_millis(10)).contains("loss 100%"));
    }

    #[test]
    fn jitter_follows_rfc3550() {
        let flat = [ms(10), ms(10), ms(10)];
        assert_eq!(rfc3550_jitter(&flat), Some(Duration::ZERO));
        let jumpy = [ms(10), ms(26)];
        let j = rfc3550_jitter(&jumpy).unwrap();
        assert!((j.as_secs_f64() - 0.001).abs() < 1e-9);
    }

    #[test]
    fn reordering_counts_late_lower_sequence_numbers() {
        let at = Duration::from_millis;
        assert_eq!(count_reordered(&[(0, at(1)), (1, at(2)), (2, at(3))]), 0);
        assert_eq!(count_reordered(&[(0, at(5)), (1, at(2)), (2, at(3))]), 1);
        assert_eq!(count_reordered(&[(2, at(1)), (0, at(2)), (1, at(3))]), 2);
    }
}
