use std::fmt::Write;
use std::io;

use haze_store::{Bucket, Store};
use serde::Serialize;

pub const MAX_COMPARE: usize = 6;
const STEPS: [i64; 16] = [
    5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000,
    3_600_000, 7_200_000, 10_800_000, 21_600_000, 43_200_000, 86_400_000,
];
const COMPARE_COLOURS: [&str; MAX_COMPARE] = [
    "#4cc9f0", "#f72585", "#ffd166", "#80ed99", "#b388ff", "#ff9e40",
];

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Point {
    pub t: i64,
    pub steps: u32,
    pub sent: u64,
    pub received: u64,
    pub refused: u64,
    pub loss: f64,
    pub min: Option<f64>,
    pub p10: Option<f64>,
    pub p25: Option<f64>,
    pub median: Option<f64>,
    pub p75: Option<f64>,
    pub p90: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Series {
    pub target: String,
    pub from: i64,
    pub to: i64,
    pub step: i64,
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl Frame {
    pub fn bare(width: u32, height: u32) -> Frame {
        Frame {
            width: width.clamp(60, 4000),
            height: height.clamp(20, 2000),
            left: 0,
            right: 0,
            top: 2,
            bottom: 2,
        }
    }

    pub fn new(width: u32, height: u32) -> Frame {
        Frame {
            width: width.clamp(200, 4000),
            height: height.clamp(120, 2000),
            left: 52,
            right: 12,
            top: 10,
            bottom: 26,
        }
    }

    fn plot_width(&self) -> f64 {
        (self.width - self.left - self.right) as f64
    }

    fn plot_height(&self) -> f64 {
        (self.height - self.top - self.bottom) as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Theme {
    Dark,
    Light,
}

struct Palette {
    background: &'static str,
    grid: &'static str,
    text: &'static str,
    smoke: &'static str,
    down: &'static str,
    refused: &'static str,
}

impl Theme {
    pub fn parse(text: Option<&str>) -> Theme {
        match text {
            Some("light") => Theme::Light,
            _ => Theme::Dark,
        }
    }

    fn palette(self) -> Palette {
        match self {
            Theme::Dark => Palette {
                background: "#11151c",
                grid: "#262d38",
                text: "#9aa5b5",
                smoke: "#c8d0dc",
                down: "#ff3b3b",
                refused: "#ffb000",
            },
            Theme::Light => Palette {
                background: "#ffffff",
                grid: "#e3e7ed",
                text: "#4a5462",
                smoke: "#36404d",
                down: "#e00000",
                refused: "#c27c00",
            },
        }
    }
}

pub fn choose_step(span_ms: i64, width_px: u32, min_step_ms: i64) -> i64 {
    let wanted = span_ms / (width_px.max(100) as i64 / 2).max(1);
    STEPS
        .iter()
        .copied()
        .find(|s| *s >= wanted.max(min_step_ms))
        .unwrap_or(86_400_000)
}

fn point_of(bucket: &Bucket) -> Point {
    let h = &bucket.histogram;
    let q = |p: f64| crate::hub::ms(h.quantile(p));
    Point {
        t: bucket.start_ms,
        steps: bucket.steps,
        sent: bucket.sent,
        received: bucket.received,
        refused: bucket.refused,
        loss: (bucket.loss() * 1e4).round() / 1e4,
        min: crate::hub::ms(h.min()),
        p10: q(0.10),
        p25: q(0.25),
        median: q(0.50),
        p75: q(0.75),
        p90: q(0.90),
        max: crate::hub::ms(h.max()),
    }
}

pub fn series(store: &Store, target: &str, from: i64, to: i64, step: i64) -> io::Result<Series> {
    let step = step.max(1000);
    let from = from.div_euclid(step) * step;
    let points = store
        .query(target, from, to, step)?
        .iter()
        .map(point_of)
        .collect();
    Ok(Series {
        target: target.to_string(),
        from,
        to,
        step,
        points,
    })
}

pub fn loss_colour(loss: f64) -> &'static str {
    match (loss * 20.0).round() as u32 {
        0 => "#26ff00",
        1 => "#00b8ff",
        2 => "#0059ff",
        3 => "#5e00ff",
        4..=5 => "#7e00ff",
        6..=10 => "#dd00ff",
        _ => "#ff0000",
    }
}

fn nice_ceiling(value: f64) -> f64 {
    let value = value.max(0.1);
    let magnitude = 10f64.powf(value.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|f| f * magnitude)
        .find(|v| *v >= value)
        .unwrap_or(10.0 * magnitude)
}

fn y_scale<'a>(points: impl Iterator<Item = &'a Point>) -> f64 {
    let mut p90s: Vec<f64> = Vec::new();
    let mut medians: Vec<f64> = Vec::new();
    for p in points {
        p90s.extend(p.p90);
        medians.extend(p.median);
    }
    if p90s.is_empty() {
        return 10.0;
    }
    p90s.sort_by(f64::total_cmp);
    medians.sort_by(f64::total_cmp);
    let high = p90s[(p90s.len() - 1) * 98 / 100];
    let median_top = *medians.last().unwrap_or(&high);
    nice_ceiling(high.max(median_top) * 1.15)
}

fn label_ms(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.1}s", v / 1000.0)
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else if v >= 10.0 || (v * 10.0).fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

const TIME_STEPS: [i64; 16] = [
    60_000,
    300_000,
    600_000,
    900_000,
    1_800_000,
    3_600_000,
    7_200_000,
    10_800_000,
    21_600_000,
    43_200_000,
    86_400_000,
    172_800_000,
    604_800_000,
    1_209_600_000,
    2_592_000_000,
    7_776_000_000,
];

fn clock_label(ms: i64, span: i64, offset_min: i64) -> String {
    let local = ms - offset_min * 60_000;
    let secs = local.div_euclid(1000);
    let day = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let date = haze_store::date_name(day);
    if span <= 2 * 86_400_000 {
        if tod == 0 {
            date[5..].to_string()
        } else {
            format!("{:02}:{:02}", tod / 3600, tod / 60 % 60)
        }
    } else if span <= 60 * 86_400_000 {
        date[5..].to_string()
    } else {
        date[..7].to_string()
    }
}

struct Plot<'a> {
    frame: Frame,
    from: i64,
    to: i64,
    y_max: f64,
    palette: &'a Palette,
}

impl Plot<'_> {
    fn x(&self, t: i64) -> f64 {
        self.frame.left as f64
            + (t - self.from) as f64 / (self.to - self.from).max(1) as f64 * self.frame.plot_width()
    }

    fn y(&self, v: f64) -> f64 {
        let top = self.frame.top as f64;
        top + self.frame.plot_height() * (1.0 - (v / self.y_max).clamp(0.0, 1.0))
    }

    fn open(&self, out: &mut String, title: &str) {
        let f = self.frame;
        let _ = write!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="DejaVu Sans Mono, monospace" font-size="11" data-from="{from}" data-to="{to}" data-ymax="{ymax}" data-left="{left}" data-right="{right}" data-top="{top}" data-bottom="{bottom}"><title>{title}</title><rect width="{w}" height="{h}" fill="{bg}"/>"#,
            w = f.width,
            h = f.height,
            left = f.left,
            right = f.right,
            top = f.top,
            bottom = f.bottom,
            from = self.from,
            to = self.to,
            ymax = self.y_max,
            title = escape(title),
            bg = self.palette.background,
        );
    }

    fn axes(&self, out: &mut String, offset_min: i64) {
        let f = self.frame;
        let (left, right) = (f.left as f64, (f.width - f.right) as f64);
        for i in 0..=4 {
            let v = self.y_max * i as f64 / 4.0;
            let y = self.y(v);
            let _ = write!(
                out,
                r#"<line x1="{left}" x2="{right}" y1="{y:.1}" y2="{y:.1}" stroke="{g}"/><text x="{tx}" y="{ty:.1}" fill="{c}" text-anchor="end">{label}</text>"#,
                g = self.palette.grid,
                c = self.palette.text,
                tx = left - 6.0,
                ty = y + 4.0,
                label = label_ms(v),
            );
        }
        let _ = write!(
            out,
            r#"<text x="4" y="{y}" fill="{c}" font-size="10">ms</text>"#,
            y = f.top + 8,
            c = self.palette.text
        );
        let span = self.to - self.from;
        let min_gap = span as f64 * 70.0 / self.frame.plot_width();
        let tick = TIME_STEPS
            .iter()
            .copied()
            .find(|s| *s as f64 >= min_gap)
            .unwrap_or(7_776_000_000);
        let offset = offset_min * 60_000;
        let mut t = (self.from - offset).div_euclid(tick) * tick + tick + offset;
        let bottom = (f.height - f.bottom) as f64;
        while t < self.to {
            let x = self.x(t);
            let _ = write!(
                out,
                r#"<line x1="{x:.1}" x2="{x:.1}" y1="{top}" y2="{bottom}" stroke="{g}"/><text x="{x:.1}" y="{ty}" fill="{c}" text-anchor="middle">{label}</text>"#,
                top = f.top,
                g = self.palette.grid,
                c = self.palette.text,
                ty = bottom + 16.0,
                label = clock_label(t, span, offset_min),
            );
            t += tick;
        }
    }
}

pub struct GraphOptions {
    pub frame: Frame,
    pub theme: Theme,
    pub offset_min: i64,
    pub bare: bool,
}

fn bucket_span(plot: &Plot, t: i64, step: i64) -> (f64, f64) {
    let x0 = plot.x(t.max(plot.from));
    let x1 = plot.x((t + step).min(plot.to));
    (x0, (x1 - x0).max(1.0))
}

pub fn smoke_svg(series: &Series, options: &GraphOptions) -> String {
    let palette = options.theme.palette();
    let plot = Plot {
        frame: options.frame,
        from: series.from,
        to: series.to,
        y_max: y_scale(series.points.iter()),
        palette: &palette,
    };
    let mut out = String::with_capacity(64 * 1024 + series.points.len() * 400);
    plot.open(&mut out, &series.target);
    if !options.bare {
        plot.axes(&mut out, options.offset_min);
    }
    let f = options.frame;
    let (top, height) = (f.top as f64, f.plot_height());
    for p in &series.points {
        let (x, w) = bucket_span(&plot, p.t, series.step);
        if p.sent > 0 && p.received == 0 {
            let colour = if p.refused >= p.sent {
                palette.refused
            } else {
                palette.down
            };
            let _ = write!(
                out,
                r#"<rect x="{x:.1}" y="{top}" width="{w:.1}" height="{height}" fill="{colour}" fill-opacity="0.22"/>"#
            );
            continue;
        }
        for (low, high, opacity) in [
            (p.min, p.max, 0.12),
            (p.p10, p.p90, 0.22),
            (p.p25, p.p75, 0.32),
        ] {
            if let (Some(low), Some(high)) = (low, high) {
                let (y0, y1) = (plot.y(high), plot.y(low));
                let _ = write!(
                    out,
                    r#"<rect x="{x:.1}" y="{y0:.1}" width="{w:.1}" height="{h:.1}" fill="{c}" fill-opacity="{opacity}"/>"#,
                    h = (y1 - y0).max(0.6),
                    c = palette.smoke
                );
            }
        }
    }
    for p in &series.points {
        let Some(median) = p.median else { continue };
        let (x, w) = bucket_span(&plot, p.t, series.step);
        let y = plot.y(median);
        let _ = write!(
            out,
            r#"<line x1="{x:.1}" x2="{x2:.1}" y1="{y:.1}" y2="{y:.1}" stroke="{c}" stroke-width="2"/>"#,
            x2 = x + w.max(2.0),
            c = loss_colour(p.loss)
        );
    }
    out.push_str("</svg>");
    out
}

pub fn compare_svg(list: &[Series], options: &GraphOptions) -> String {
    let palette = options.theme.palette();
    let Some(first) = list.first() else {
        let plot = Plot {
            frame: options.frame,
            from: 0,
            to: 1,
            y_max: 10.0,
            palette: &palette,
        };
        let mut out = String::new();
        plot.open(&mut out, "compare");
        out.push_str("</svg>");
        return out;
    };
    let plot = Plot {
        frame: options.frame,
        from: first.from,
        to: first.to,
        y_max: y_scale(list.iter().flat_map(|s| s.points.iter())),
        palette: &palette,
    };
    let title: Vec<&str> = list.iter().map(|s| s.target.as_str()).collect();
    let mut out = String::new();
    plot.open(&mut out, &title.join(", "));
    plot.axes(&mut out, options.offset_min);
    for (series, colour) in list.iter().zip(COMPARE_COLOURS) {
        for p in &series.points {
            if let (Some(low), Some(high)) = (p.p10, p.p90) {
                let (x, w) = bucket_span(&plot, p.t, series.step);
                let (y0, y1) = (plot.y(high), plot.y(low));
                let _ = write!(
                    out,
                    r#"<rect x="{x:.1}" y="{y0:.1}" width="{w:.1}" height="{h:.1}" fill="{colour}" fill-opacity="0.13"/>"#,
                    h = (y1 - y0).max(0.6)
                );
            }
        }
    }
    for (series, colour) in list.iter().zip(COMPARE_COLOURS) {
        let mut path = String::new();
        let mut pen_down = false;
        let mut last_t = i64::MIN;
        for p in &series.points {
            let Some(median) = p.median else {
                pen_down = false;
                continue;
            };
            if last_t != i64::MIN && p.t - last_t > series.step * 2 {
                pen_down = false;
            }
            let (x, w) = bucket_span(&plot, p.t, series.step);
            let y = plot.y(median);
            let _ = write!(
                path,
                "{}{:.1},{:.1}L{:.1},{:.1}",
                if pen_down { "L" } else { "M" },
                x,
                y,
                x + w,
                y
            );
            pen_down = true;
            last_t = p.t;
        }
        let _ = write!(
            out,
            r#"<path d="{path}" fill="none" stroke="{colour}" stroke-width="2"/>"#
        );
    }
    out.push_str("</svg>");
    out
}

pub fn compare_colours() -> &'static [&'static str] {
    &COMPARE_COLOURS
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

static FONT: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

pub fn png(svg: &str) -> Result<Vec<u8>, String> {
    let mut options = resvg::usvg::Options {
        font_family: "DejaVu Sans Mono".into(),
        ..Default::default()
    };
    options.fontdb_mut().load_font_data(FONT.to_vec());
    let tree = resvg::usvg::Tree::from_str(svg, &options).map_err(|e| e.to_string())?;
    let size = tree.size().to_int_size();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(size.width(), size.height()).ok_or("graph has no size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_the_range_and_width() {
        assert_eq!(choose_step(3 * 3_600_000, 900, 60_000), 60_000);
        assert_eq!(choose_step(86_400_000, 900, 60_000), 300_000);
        assert_eq!(choose_step(7 * 86_400_000, 900, 60_000), 1_800_000);
        assert_eq!(choose_step(30 * 86_400_000, 900, 60_000), 7_200_000);
        assert_eq!(choose_step(400 * 86_400_000, 900, 60_000), 86_400_000);
        assert_eq!(choose_step(600_000, 900, 5_000), 5_000);
        assert_eq!(choose_step(16 * 60_000, 1140, 15_000), 15_000);
    }

    #[test]
    fn loss_colours_follow_smokeping() {
        assert_eq!(loss_colour(0.0), "#26ff00");
        assert_eq!(loss_colour(0.05), "#00b8ff");
        assert_eq!(loss_colour(0.5), "#dd00ff");
        assert_eq!(loss_colour(1.0), "#ff0000");
    }

    #[test]
    fn scale_ignores_rare_spikes() {
        let point = |p90: f64| Point {
            t: 0,
            steps: 1,
            sent: 20,
            received: 20,
            refused: 0,
            loss: 0.0,
            min: Some(1.0),
            p10: Some(1.0),
            p25: Some(1.0),
            median: Some(4.0),
            p75: Some(5.0),
            p90: Some(p90),
            max: Some(p90 * 3.0),
        };
        let mut points: Vec<Point> = (0..200).map(|_| point(6.0)).collect();
        points.push(point(900.0));
        assert_eq!(y_scale(points.iter()), 10.0);
    }

    #[test]
    fn axis_labels_keep_quarter_steps() {
        assert_eq!(label_ms(12.5), "12.5");
        assert_eq!(label_ms(37.5), "37.5");
        assert_eq!(label_ms(50.0), "50");
        assert_eq!(label_ms(0.625), "0.62");
        assert_eq!(label_ms(2.5), "2.5");
        assert_eq!(label_ms(1500.0), "1.5s");
    }

    #[test]
    fn labels_use_local_offset() {
        assert_eq!(clock_label(3_600_000, 3_600_000 * 3, 0), "01:00");
        assert_eq!(clock_label(3_600_000, 3_600_000 * 3, -60), "02:00");
        assert_eq!(clock_label(0, 86_400_000 * 30, 0), "01-01");
        assert_eq!(clock_label(0, 86_400_000 * 400, 0), "1970-01");
    }
}
