use std::collections::VecDeque;
use std::time::Duration;

use crate::duration::parse_duration;
use crate::sample::Summary;

pub const DEFAULT_BASELINE_STEPS: usize = 288;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Metric {
    Loss,
    Refused,
    Min,
    Mean,
    Median,
    Max,
    Jitter,
    Smoke,
    Percentile(u8),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Threshold {
    Millis(f64),
    Percent(f64),
    Baseline(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub hits: usize,
    pub of: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternItem {
    Compare(Op, f64),
    Any,
    Unknown,
    NotUnknown,
    Skip(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PatternKind {
    Loss,
    Rtt,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Matcher {
    Threshold {
        metric: Metric,
        op: Op,
        threshold: Threshold,
        window: Window,
        baseline_steps: usize,
    },
    Pattern {
        kind: PatternKind,
        items: Vec<PatternItem>,
    },
}

fn parse_op(text: &str) -> Option<(Op, &str)> {
    for (token, op) in [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("==", Op::Eq),
        ("!=", Op::Ne),
        ("=!", Op::Ne),
        (">", Op::Gt),
        ("<", Op::Lt),
    ] {
        if let Some(rest) = text.strip_prefix(token) {
            return Some((op, rest));
        }
    }
    None
}

fn parse_metric(word: &str) -> Result<Metric, String> {
    Ok(match word {
        "loss" => Metric::Loss,
        "refused" => Metric::Refused,
        "min" => Metric::Min,
        "mean" | "avg" => Metric::Mean,
        "median" | "med" => Metric::Median,
        "max" => Metric::Max,
        "jitter" => Metric::Jitter,
        "smoke" => Metric::Smoke,
        other => match other.strip_prefix('p').and_then(|n| n.parse::<u8>().ok()) {
            Some(p) if (1..=99).contains(&p) => Metric::Percentile(p),
            _ => return Err(format!("`{other}` is not a metric")),
        },
    })
}

fn parse_threshold(text: &str, metric: Metric) -> Result<Threshold, String> {
    let text = text.trim();
    if let Some(factor) = text
        .strip_suffix("baseline")
        .map(str::trim)
        .and_then(|t| t.strip_suffix('x'))
        .or_else(|| text.strip_suffix('x'))
    {
        let factor: f64 = factor
            .trim()
            .parse()
            .map_err(|_| format!("`{text}` is not a factor like 2x baseline"))?;
        return Ok(Threshold::Baseline(factor));
    }
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent
            .trim()
            .parse()
            .map_err(|_| format!("`{text}` is not a percentage"))?;
        if metric != Metric::Loss {
            return Err(format!("a percentage only applies to loss, not `{text}`"));
        }
        return Ok(Threshold::Percent(value));
    }
    if metric == Metric::Loss {
        return Err(format!("loss takes a percentage like 10%, not `{text}`"));
    }
    if metric == Metric::Refused {
        let count: u32 = text
            .parse()
            .map_err(|_| format!("refused takes a count like 1, not `{text}`"))?;
        return Ok(Threshold::Millis(count as f64));
    }
    let value = parse_duration(text)?;
    Ok(Threshold::Millis(value.as_secs_f64() * 1e3))
}

fn parse_window(words: &[&str]) -> Result<Window, String> {
    match words {
        [] => Ok(Window { hits: 1, of: 1 }),
        ["for", n] | ["for", n, "steps" | "step"] => {
            let n: usize = n.parse().map_err(|_| format!("`{n}` is not a count"))?;
            Ok(Window { hits: n, of: n })
        }
        [k, "of", n] | [k, "of", n, "steps" | "step"] => {
            let k: usize = k.parse().map_err(|_| format!("`{k}` is not a count"))?;
            let n: usize = n.parse().map_err(|_| format!("`{n}` is not a count"))?;
            Ok(Window { hits: k, of: n })
        }
        other => Err(format!(
            "`{}` is not a window like `for 3 steps` or `3 of 5`",
            other.join(" ")
        )),
    }
    .and_then(|w| {
        if w.hits == 0 || w.hits > w.of {
            Err(format!("window {} of {} cannot match", w.hits, w.of))
        } else {
            Ok(w)
        }
    })
}

fn parse_pattern_item(item: &str, kind: PatternKind) -> Result<PatternItem, String> {
    let item = item.trim();
    if item == "*" {
        return Ok(PatternItem::Any);
    }
    if let Some(n) = item.strip_prefix('*').and_then(|i| i.strip_suffix('*')) {
        return n
            .parse()
            .map(PatternItem::Skip)
            .map_err(|_| format!("`{item}` is not a skip like *3*"));
    }
    let (op, rest) = parse_op(item).ok_or_else(|| format!("`{item}` has no comparison"))?;
    if rest == "U" {
        return match op {
            Op::Eq => Ok(PatternItem::Unknown),
            Op::Ne => Ok(PatternItem::NotUnknown),
            _ => Err(format!("`{item}`: U only works with == and !=")),
        };
    }
    if rest == "S" {
        return Err("==S (start of run) is not supported".into());
    }
    let number = match kind {
        PatternKind::Loss => rest
            .strip_suffix('%')
            .ok_or_else(|| format!("loss pattern item `{item}` needs a %"))?,
        PatternKind::Rtt => rest,
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("`{item}` is not a number"))?;
    Ok(PatternItem::Compare(op, value))
}

impl std::str::FromStr for Matcher {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        for (prefix, kind) in [
            ("loss pattern", PatternKind::Loss),
            ("rtt pattern", PatternKind::Rtt),
        ] {
            if let Some(rest) = text.strip_prefix(prefix) {
                let items = rest
                    .split(',')
                    .map(|i| parse_pattern_item(i, kind))
                    .collect::<Result<Vec<_>, _>>()?;
                if !items.iter().any(|i| !matches!(i, PatternItem::Skip(_))) {
                    return Err("a pattern needs at least one value".into());
                }
                return Ok(Matcher::Pattern { kind, items });
            }
        }
        let op_at = text
            .find(['<', '>', '=', '!'])
            .ok_or_else(|| format!("`{text}` has no comparison like > or >="))?;
        let metric = parse_metric(text[..op_at].trim())?;
        let (op, rest) = parse_op(&text[op_at..])
            .ok_or_else(|| format!("`{}` is not a comparison", &text[op_at..]))?;
        let words: Vec<&str> = rest.split_whitespace().collect();
        let window_at = words
            .iter()
            .position(|w| *w == "for")
            .or_else(|| {
                words
                    .iter()
                    .position(|w| *w == "of")
                    .map(|i| i.saturating_sub(1))
            })
            .unwrap_or(words.len());
        let threshold = parse_threshold(&words[..window_at].join(" "), metric)?;
        let window = parse_window(&words[window_at..])?;
        Ok(Matcher::Threshold {
            metric,
            op,
            threshold,
            window,
            baseline_steps: DEFAULT_BASELINE_STEPS,
        })
    }
}

pub fn metric_ms(summary: &Summary, metric: Metric) -> Option<f64> {
    let ms = |d: Option<Duration>| d.map(|d| d.as_secs_f64() * 1e3);
    match metric {
        Metric::Loss => Some(summary.loss * 100.0),
        Metric::Refused => Some(summary.refused as f64),
        Metric::Min => ms(summary.min),
        Metric::Mean => ms(summary.mean),
        Metric::Median => ms(summary.median),
        Metric::Max => ms(summary.max),
        Metric::Jitter => ms(summary.jitter),
        Metric::Smoke => ms(summary.smoke()),
        Metric::Percentile(p) => ms(match p {
            10 => summary.p10,
            25 => summary.p25,
            50 => summary.median,
            75 => summary.p75,
            90 => summary.p90,
            99 => summary.p99,
            _ => return None,
        }),
    }
}

fn compare(op: Op, value: f64, threshold: f64) -> bool {
    match op {
        Op::Gt => value > threshold,
        Op::Ge => value >= threshold,
        Op::Lt => value < threshold,
        Op::Le => value <= threshold,
        Op::Eq => value == threshold,
        Op::Ne => value != threshold,
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[(values.len() - 1) / 2])
}

fn pattern_value(summary: &Summary, kind: PatternKind) -> Option<f64> {
    match kind {
        PatternKind::Loss => (summary.sent > 0).then_some(summary.loss * 100.0),
        PatternKind::Rtt => metric_ms(summary, Metric::Median),
    }
}

fn pattern_matches(items: &[PatternItem], values: &[Option<f64>]) -> bool {
    let Some((last, rest)) = items.split_last() else {
        return true;
    };
    match last {
        PatternItem::Skip(n) => (0..=*n.min(&values.len()))
            .any(|skip| pattern_matches(rest, &values[..values.len() - skip])),
        item => {
            let Some((value, earlier)) = values.split_last() else {
                return false;
            };
            let ok = match (item, value) {
                (PatternItem::Any, _) => true,
                (PatternItem::Unknown, v) => v.is_none(),
                (PatternItem::NotUnknown, v) => v.is_some(),
                (PatternItem::Compare(op, t), Some(v)) => compare(*op, *v, *t),
                _ => false,
            };
            ok && pattern_matches(rest, earlier)
        }
    }
}

impl Matcher {
    pub fn with_baseline_steps(mut self, steps: usize) -> Self {
        if let Matcher::Threshold { baseline_steps, .. } = &mut self {
            *baseline_steps = steps;
        }
        self
    }

    pub fn matches(&self, series: &[Summary]) -> bool {
        match self {
            Matcher::Pattern { kind, items } => {
                let values: Vec<Option<f64>> =
                    series.iter().map(|s| pattern_value(s, *kind)).collect();
                pattern_matches(items, &values)
            }
            Matcher::Threshold {
                metric,
                op,
                threshold,
                window,
                baseline_steps,
            } => {
                if series.len() < window.of {
                    return false;
                }
                let (history, recent) = series.split_at(series.len() - window.of);
                let limit = match threshold {
                    Threshold::Millis(v) | Threshold::Percent(v) => *v,
                    Threshold::Baseline(factor) => {
                        let from = history.len().saturating_sub(*baseline_steps);
                        let values: Vec<f64> = history[from..]
                            .iter()
                            .filter_map(|s| metric_ms(s, *metric))
                            .collect();
                        if values.len() < window.of {
                            return false;
                        }
                        match median(values) {
                            Some(b) => b * factor,
                            None => return false,
                        }
                    }
                };
                let hits = recent
                    .iter()
                    .filter(|s| metric_ms(s, *metric).is_some_and(|v| compare(*op, v, limit)))
                    .count();
                hits >= window.hits
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlertPolicy {
    pub clear_after: usize,
    pub flap_window: usize,
    pub flap_limit: usize,
}

impl Default for AlertPolicy {
    fn default() -> Self {
        Self {
            clear_after: 1,
            flap_window: 20,
            flap_limit: 6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlertEvent {
    Raised,
    Cleared,
    Flapping,
    Settled { active: bool },
}

#[derive(Clone, Debug, Default)]
pub struct AlertState {
    pub active: bool,
    pub flapping: bool,
    quiet: usize,
    changes: VecDeque<bool>,
}

impl AlertState {
    pub fn update(&mut self, matched: bool, policy: &AlertPolicy) -> Option<AlertEvent> {
        let was = self.active;
        if matched {
            self.quiet = 0;
            self.active = true;
        } else if self.active {
            self.quiet += 1;
            if self.quiet >= policy.clear_after {
                self.active = false;
                self.quiet = 0;
            }
        }
        let changed = was != self.active;
        self.changes.push_back(changed);
        while self.changes.len() > policy.flap_window {
            self.changes.pop_front();
        }
        let recent_changes = self.changes.iter().filter(|c| **c).count();
        if !self.flapping && recent_changes >= policy.flap_limit {
            self.flapping = true;
            return Some(AlertEvent::Flapping);
        }
        if self.flapping {
            if recent_changes == 0 {
                self.flapping = false;
                return Some(AlertEvent::Settled {
                    active: self.active,
                });
            }
            return None;
        }
        match (was, self.active) {
            (false, true) => Some(AlertEvent::Raised),
            (true, false) => Some(AlertEvent::Cleared),
            _ => None,
        }
    }
}

pub fn held_by_dependency<'a>(
    depends: impl IntoIterator<Item = &'a str>,
    is_down: impl Fn(&str) -> bool,
) -> bool {
    depends.into_iter().any(is_down)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(loss: f64, median_ms: Option<f64>) -> Summary {
        let d = median_ms.map(|m| Duration::from_secs_f64(m / 1e3));
        Summary {
            sent: 20,
            received: (20.0 * (1.0 - loss)) as u32,
            refused: 0,
            loss,
            min: d,
            p10: d,
            p25: d,
            median: d,
            p75: d,
            p90: d,
            p99: d,
            max: d,
            mean: d,
            jitter: d.map(|_| Duration::ZERO),
            pdv: d.map(|_| Duration::ZERO),
        }
    }

    fn rtt(values: &[f64]) -> Vec<Summary> {
        values.iter().map(|v| step(0.0, Some(*v))).collect()
    }

    fn loss(values: &[f64]) -> Vec<Summary> {
        values
            .iter()
            .map(|l| step(*l / 100.0, (*l < 100.0).then_some(10.0)))
            .collect()
    }

    fn m(text: &str) -> Matcher {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn parses_the_documented_forms() {
        assert_eq!(
            m("median > 20ms 3 of 5"),
            Matcher::Threshold {
                metric: Metric::Median,
                op: Op::Gt,
                threshold: Threshold::Millis(20.0),
                window: Window { hits: 3, of: 5 },
                baseline_steps: DEFAULT_BASELINE_STEPS,
            }
        );
        assert_eq!(
            m("loss >= 10% for 3 steps"),
            Matcher::Threshold {
                metric: Metric::Loss,
                op: Op::Ge,
                threshold: Threshold::Percent(10.0),
                window: Window { hits: 3, of: 3 },
                baseline_steps: DEFAULT_BASELINE_STEPS,
            }
        );
        assert!(matches!(
            m("smoke > 2x baseline"),
            Matcher::Threshold {
                threshold: Threshold::Baseline(2.0),
                window: Window { hits: 1, of: 1 },
                ..
            }
        ));
        assert!(matches!(
            m("p90>1.5x baseline for 2 steps"),
            Matcher::Threshold {
                metric: Metric::Percentile(90),
                ..
            }
        ));
        assert_eq!(
            m("loss pattern ==0%,*2*,>20%,==U"),
            Matcher::Pattern {
                kind: PatternKind::Loss,
                items: vec![
                    PatternItem::Compare(Op::Eq, 0.0),
                    PatternItem::Skip(2),
                    PatternItem::Compare(Op::Gt, 20.0),
                    PatternItem::Unknown,
                ],
            }
        );
    }

    #[test]
    fn rejects_bad_rules_with_a_reason() {
        for bad in [
            "median 20ms",
            "loss > 20ms",
            "median > 10%",
            "wobble > 3ms",
            "median > 20ms 6 of 5",
            "median > 20ms for 0 steps",
            "loss pattern >20",
            "rtt pattern ==S,==U",
            "rtt pattern *3*",
        ] {
            assert!(bad.parse::<Matcher>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn refused_rule_counts_refusals_and_loss_rule_ignores_them() {
        let refused = |n: u32| Summary {
            sent: 5,
            received: 0,
            refused: n,
            loss: if n == 5 { 0.0 } else { 1.0 },
            ..step(0.0, None)
        };
        let series = vec![refused(5), refused(5), refused(5)];
        assert!(m("refused >= 1 for 3 steps").matches(&series));
        assert!(!m("loss >= 10% for 3 steps").matches(&series));
        let silent = vec![refused(0), refused(0), refused(0)];
        assert!(!m("refused >= 1 for 3 steps").matches(&silent));
        assert!(m("loss >= 10% for 3 steps").matches(&silent));
        assert!("refused >= 10%".parse::<Matcher>().is_err());
    }

    #[test]
    fn three_of_five() {
        let rule = m("median > 20ms 3 of 5");
        assert!(rule.matches(&rtt(&[5.0, 25.0, 5.0, 30.0, 5.0, 40.0])));
        assert!(!rule.matches(&rtt(&[25.0, 25.0, 5.0, 30.0, 5.0, 5.0])));
        assert!(!rule.matches(&rtt(&[25.0, 25.0, 25.0, 25.0])));
        assert!(rule.matches(&rtt(&[25.0, 25.0, 25.0, 25.0, 25.0])));
    }

    #[test]
    fn loss_streaks() {
        let rule = m("loss >= 10% for 3 steps");
        assert!(rule.matches(&loss(&[0.0, 10.0, 50.0, 100.0])));
        assert!(!rule.matches(&loss(&[10.0, 10.0, 0.0, 10.0, 10.0])));
        assert!(!rule.matches(&loss(&[0.0, 5.0, 5.0, 5.0])));
    }

    #[test]
    fn total_loss_does_not_count_as_low_latency_or_high_latency() {
        let rule = m("median < 1ms for 2 steps");
        assert!(!rule.matches(&loss(&[100.0, 100.0])));
    }

    #[test]
    fn baseline_uses_history_before_the_window() {
        let rule = m("median > 2x baseline for 2 steps");
        let mut series = rtt(&[10.0; 10]);
        series.extend(rtt(&[25.0, 25.0]));
        assert!(rule.matches(&series));
        let mut quiet = rtt(&[10.0; 10]);
        quiet.extend(rtt(&[25.0, 15.0]));
        assert!(!rule.matches(&quiet));
        assert!(
            !rule.matches(&rtt(&[25.0, 25.0])),
            "no history, no baseline"
        );
        let short = m("median > 2x baseline").with_baseline_steps(3);
        assert!(short.matches(&rtt(&[100.0, 100.0, 100.0, 10.0, 10.0, 10.0, 25.0])));
    }

    #[test]
    fn smokeping_patterns() {
        let jump = m("rtt pattern <10,<10,>100,>100");
        assert!(jump.matches(&rtt(&[50.0, 5.0, 5.0, 150.0, 200.0])));
        assert!(!jump.matches(&rtt(&[5.0, 150.0, 5.0, 200.0])));
        let twice = m("loss pattern >10%,*10*,>10%");
        assert!(twice.matches(&loss(&[20.0, 0.0, 0.0, 0.0, 0.0, 0.0, 30.0])));
        assert!(twice.matches(&loss(&[20.0, 30.0])));
        assert!(!twice.matches(&loss(&[
            20.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 30.0
        ])));
        let down = m("rtt pattern !=U,==U,==U");
        assert!(down.matches(&loss(&[0.0, 100.0, 100.0])));
        assert!(!down.matches(&loss(&[100.0, 100.0, 100.0])));
        let any = m("loss pattern *,>50%");
        assert!(any.matches(&loss(&[100.0, 60.0])));
        assert!(!any.matches(&loss(&[60.0])));
    }

    fn run(states: &[bool], policy: &AlertPolicy) -> Vec<Option<AlertEvent>> {
        let mut state = AlertState::default();
        states.iter().map(|m| state.update(*m, policy)).collect()
    }

    #[test]
    fn raises_and_clears_once() {
        let policy = AlertPolicy::default();
        let events = run(&[false, true, true, true, false, false], &policy);
        assert_eq!(
            events,
            vec![
                None,
                Some(AlertEvent::Raised),
                None,
                None,
                Some(AlertEvent::Cleared),
                None
            ]
        );
    }

    #[test]
    fn clear_after_holds_the_alert_through_short_gaps() {
        let policy = AlertPolicy {
            clear_after: 3,
            ..AlertPolicy::default()
        };
        let events = run(&[true, false, false, true, false, false, false], &policy);
        assert_eq!(
            events,
            vec![
                Some(AlertEvent::Raised),
                None,
                None,
                None,
                None,
                None,
                Some(AlertEvent::Cleared)
            ]
        );
    }

    #[test]
    fn flapping_is_reported_once_then_silenced_until_it_settles() {
        let policy = AlertPolicy {
            clear_after: 1,
            flap_window: 6,
            flap_limit: 4,
        };
        let mut series = vec![true, false, true, false];
        series.extend([true, false, true, false]);
        series.extend([true; 7]);
        let events = run(&series, &policy);
        let notified: Vec<AlertEvent> = events.into_iter().flatten().collect();
        assert_eq!(
            notified,
            vec![
                AlertEvent::Raised,
                AlertEvent::Cleared,
                AlertEvent::Raised,
                AlertEvent::Flapping,
                AlertEvent::Settled { active: true },
            ]
        );
    }

    #[test]
    fn children_are_held_while_a_dependency_is_down() {
        let down = ["router"];
        let is_down = |name: &str| down.contains(&name);
        assert!(held_by_dependency(["router"], is_down));
        assert!(!held_by_dependency(["dns"], is_down));
        assert!(!held_by_dependency([], is_down));
    }
}
