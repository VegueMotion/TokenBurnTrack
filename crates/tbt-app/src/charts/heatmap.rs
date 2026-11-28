use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Hitbox, Hsla, IntoElement, ParentElement, Pixels, SharedString,
    Styled, Window, canvas, div, fill, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, Sizable, h_flex, v_flex};
use jiff::ToSpan;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use tbt_core::TimeRange;
use tbt_store::CostBucket;

use super::format::{fmt_count, fmt_tokens, fmt_usd, micros_to_usd};
use super::paint::{
    Align, ChartTheme, Hover, LABEL_SIZE, bounds_size, insert_hitbox, paint_text, paint_tooltip,
    rect_bounds,
};
use super::scale::Rect;
use crate::palette::heat_color;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKS: i64 = 53;
const OUTLIER_MIN_CELLS: usize = 8;
const OUTLIER_QUANTILE: f64 = 0.95;
const OUTLIER_RATIO: f64 = 2.0;
const LEGEND_STEPS: usize = 5;

const LEFT: f32 = 32.0;
const RIGHT: f32 = 4.0;
const TOP: f32 = 18.0;
const MIN_STEP: f32 = 11.0;
const MAX_STEP: f32 = 18.0;
const GAP: f32 = 3.0;
const MONTH_LABEL_WIDTH: f32 = 28.0;
pub const HEATMAP_HEIGHT: f32 = TOP + 7.0 * MAX_STEP;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeatMetric {
    #[default]
    Cost,
    Tokens,
    Calls,
}

impl HeatMetric {
    pub const ALL: [HeatMetric; 3] = [HeatMetric::Cost, HeatMetric::Tokens, HeatMetric::Calls];

    pub fn label(self) -> &'static str {
        match self {
            HeatMetric::Cost => "Cost",
            HeatMetric::Tokens => "Tokens",
            HeatMetric::Calls => "Calls",
        }
    }

    pub fn fmt(self, v: f64) -> String {
        match self {
            HeatMetric::Cost => fmt_usd(v),
            HeatMetric::Tokens => fmt_tokens(v.max(0.0).round() as u64),
            HeatMetric::Calls => fmt_count(v.max(0.0).round() as u64),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeatScale {
    pub max: f64,
    pub clamped: bool,
}

impl HeatScale {
    pub fn of(values: impl IntoIterator<Item = f64>) -> HeatScale {
        let mut nz: Vec<f64> = values
            .into_iter()
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        nz.sort_by(f64::total_cmp);
        let Some(&max) = nz.last() else {
            return HeatScale {
                max: 0.0,
                clamped: false,
            };
        };
        if nz.len() >= OUTLIER_MIN_CELLS {
            let q = nz[((nz.len() - 1) as f64 * OUTLIER_QUANTILE).floor() as usize];
            if max > q * OUTLIER_RATIO {
                return HeatScale {
                    max: q,
                    clamped: true,
                };
            }
        }
        HeatScale {
            max,
            clamped: false,
        }
    }

    pub fn intensity(&self, v: f64) -> f32 {
        intensity(v, self.max)
    }
}

pub fn intensity(v: f64, max: f64) -> f32 {
    if max <= 0.0 || v <= 0.0 || !v.is_finite() {
        0.0
    } else {
        (v / max).sqrt().clamp(0.0, 1.0) as f32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeatmapChart {
    pub first: Date,
    pub cost: Vec<f64>,
    pub tokens: Vec<u64>,
    pub calls: Vec<u64>,
}

impl HeatmapChart {
    pub fn days_ending(last: Date) -> (Date, usize) {
        let start = last.checked_sub((WEEKS * 7 - 7).days()).unwrap_or(last);
        let first = start
            .checked_sub(i64::from(start.weekday().to_monday_zero_offset()).days())
            .unwrap_or(start);
        let len = days_between(first, last).map_or(1, |d| d + 1);
        (first, len)
    }

    pub fn range_ending(last: Date, tz: &TimeZone) -> Option<TimeRange> {
        let (first, _) = Self::days_ending(last);
        let start = first.to_zoned_datetime(tz.clone()).ok()?;
        let end = last
            .checked_add(1.day())
            .ok()?
            .to_zoned_datetime(tz.clone())
            .ok()?;
        Some(TimeRange::new(start.timestamp(), end.timestamp()))
    }

    pub fn from_buckets(buckets: &[CostBucket], last: Date, tz: &TimeZone) -> HeatmapChart {
        let (first, len) = Self::days_ending(last);
        let mut chart = HeatmapChart {
            first,
            cost: vec![0.0; len],
            tokens: vec![0; len],
            calls: vec![0; len],
        };
        for b in buckets {
            let date = b.start.to_zoned_datetime(tz.clone()).date();
            if let Some(i) = days_between(first, date).filter(|i| *i < len) {
                chart.cost[i] += micros_to_usd(b.cost);
                chart.tokens[i] += b.tokens.total();
                chart.calls[i] += b.events;
            }
        }
        chart
    }

    pub fn weeks(&self) -> usize {
        self.calls.len().div_ceil(7)
    }

    pub fn day_index(&self, weekday: usize, week: usize) -> Option<usize> {
        let i = week * 7 + weekday;
        (weekday < 7 && i < self.calls.len()).then_some(i)
    }

    pub fn date(&self, index: usize) -> Option<Date> {
        self.first
            .checked_add(i64::try_from(index).ok()?.days())
            .ok()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.iter().all(|c| *c == 0) && self.cost.iter().all(|c| *c <= 0.0)
    }

    pub fn value(&self, metric: HeatMetric, index: usize) -> f64 {
        match metric {
            HeatMetric::Cost => self.cost.get(index).copied().unwrap_or(0.0),
            HeatMetric::Tokens => self.tokens.get(index).copied().unwrap_or(0) as f64,
            HeatMetric::Calls => self.calls.get(index).copied().unwrap_or(0) as f64,
        }
    }

    pub fn scale(&self, metric: HeatMetric) -> HeatScale {
        HeatScale::of((0..self.calls.len()).map(|i| self.value(metric, i)))
    }

    pub fn tooltip(&self, index: usize) -> String {
        let calls = self.calls.get(index).copied().unwrap_or(0);
        let day = self.date(index).map_or_else(
            || "?".to_owned(),
            |d| d.strftime("%a %b %-d, %Y").to_string(),
        );
        format!(
            "{day} · {} · {} tok · {} {}",
            fmt_usd(self.value(HeatMetric::Cost, index)),
            fmt_tokens(self.tokens.get(index).copied().unwrap_or(0)),
            fmt_count(calls),
            if calls == 1 { "call" } else { "calls" }
        )
    }

    pub fn month_labels(&self, first_week: usize) -> Vec<(usize, &'static str)> {
        let mut out: Vec<(usize, &'static str)> = Vec::new();
        let mut prev = None;
        for week in first_week..self.weeks() {
            let Some(month) = self.date(week * 7).map(|d| d.month()) else {
                continue;
            };
            if prev != Some(month) {
                out.push((week, MONTHS[(month - 1) as usize]));
            }
            prev = Some(month);
        }
        if out.len() >= 2 && out[1].0 - out[0].0 < 3 {
            out.remove(0);
        }
        out
    }

    pub fn view(
        &self,
        metric: Rc<Cell<HeatMetric>>,
        hover: Hover,
        theme: ChartTheme,
        cx: &App,
    ) -> AnyElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        if self.is_empty() {
            return div()
                .h(px(HEATMAP_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("No activity in this period")
                .into_any_element();
        }
        let current = metric.get();
        let toggle = h_flex()
            .gap_1()
            .children(HeatMetric::ALL.into_iter().map(|m| {
                let cell = metric.clone();
                Button::new(SharedString::from(format!("heat-metric-{}", m.label())))
                    .xsmall()
                    .ghost()
                    .selected(current == m)
                    .label(m.label())
                    .on_click(move |_, window, _| {
                        cell.set(m);
                        window.refresh();
                    })
            }));
        v_flex()
            .gap_2()
            .child(h_flex().justify_end().child(toggle))
            .child(
                heatmap_chart(self.clone(), current, hover, theme)
                    .w_full()
                    .h(px(HEATMAP_HEIGHT)),
            )
            .child(self.legend(current, theme, muted))
            .into_any_element()
    }

    fn legend(&self, metric: HeatMetric, theme: ChartTheme, muted: Hsla) -> impl IntoElement {
        let scale = self.scale(metric);
        let top = format!(
            "{}{}",
            if scale.clamped { "≥ " } else { "" },
            metric.fmt(scale.max)
        );
        let swatch = |color: Hsla| div().size(px(10.)).rounded(px(2.)).bg(color);
        h_flex()
            .gap_1()
            .items_center()
            .justify_end()
            .text_xs()
            .text_color(muted)
            .child(swatch(empty_color(&theme)))
            .child(div().mr_2().child("0"))
            .children(
                (0..LEGEND_STEPS).map(|i| swatch(heat_color(i as f32 / (LEGEND_STEPS - 1) as f32))),
            )
            .child(div().ml_1().child(top))
    }
}

fn days_between(from: Date, to: Date) -> Option<usize> {
    let days = from.until(to).ok()?.get_days();
    usize::try_from(days).ok()
}

fn empty_color(theme: &ChartTheme) -> Hsla {
    theme.grid.opacity(0.5)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeatLayout {
    pub step: f32,
    pub first_week: usize,
    pub weeks: usize,
}

impl HeatLayout {
    pub fn new(width: f32, total_weeks: usize) -> Self {
        let avail = (width - LEFT - RIGHT).max(0.0);
        let fit = (avail / MIN_STEP).floor() as usize;
        let weeks = fit.clamp(1, total_weeks.max(1));
        let step = (avail / weeks as f32).min(MAX_STEP);
        HeatLayout {
            step,
            first_week: total_weeks.saturating_sub(weeks),
            weeks,
        }
    }

    pub fn column_x(&self, week: usize) -> f32 {
        LEFT + (week - self.first_week) as f32 * self.step
    }

    pub fn row_center(&self, weekday: usize) -> f32 {
        TOP + (weekday as f32 + 0.5) * self.step - GAP / 2.0
    }

    pub fn cell(&self, weekday: usize, week: usize) -> Rect {
        let size = (self.step - GAP).max(1.0);
        Rect {
            x: self.column_x(week),
            y: TOP + weekday as f32 * self.step,
            w: size,
            h: size,
        }
    }

    pub fn cell_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        if x < LEFT || y < TOP || self.step <= 0.0 {
            return None;
        }
        let col = ((x - LEFT) / self.step) as usize;
        let row = ((y - TOP) / self.step) as usize;
        (col < self.weeks && row < 7).then_some((row, self.first_week + col))
    }
}

pub fn heatmap_chart(
    chart: HeatmapChart,
    metric: HeatMetric,
    hover: Hover,
    theme: ChartTheme,
) -> impl IntoElement + Styled {
    canvas(
        |bounds, window, _cx| insert_hitbox(bounds, window),
        move |bounds, hitbox, window, cx| {
            paint(&chart, metric, &hover, &theme, bounds, &hitbox, window, cx)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn paint(
    chart: &HeatmapChart,
    metric: HeatMetric,
    hover: &Hover,
    theme: &ChartTheme,
    bounds: Bounds<Pixels>,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = bounds.origin;
    let (w, h) = bounds_size(bounds);
    let layout = HeatLayout::new(w, chart.weeks());
    let scale = chart.scale(metric);
    let hovered = hover
        .get()
        .and_then(|(r, c)| Some(((r, c), chart.day_index(r, c)?)));

    for week in layout.first_week..chart.weeks() {
        for weekday in 0..7 {
            let Some(i) = chart.day_index(weekday, week) else {
                continue;
            };
            let v = chart.value(metric, i);
            let color = if v > 0.0 {
                heat_color(scale.intensity(v))
            } else {
                empty_color(theme)
            };
            let mut q =
                fill(rect_bounds(origin, layout.cell(weekday, week)), color).corner_radii(px(2.));
            if hovered.is_some_and(|(rc, _)| rc == (weekday, week)) {
                q = q.border_widths(px(1.5)).border_color(theme.foreground);
            }
            window.paint_quad(q);
        }
    }
    for weekday in [0, 2, 4] {
        paint_text(
            WEEKDAYS[weekday],
            origin,
            (LEFT - 6.0, layout.row_center(weekday)),
            Align::Right,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }
    let mut last_x = f32::NEG_INFINITY;
    for (week, month) in chart.month_labels(layout.first_week) {
        let x = layout.column_x(week);
        if x - last_x < MONTH_LABEL_WIDTH {
            continue;
        }
        last_x = x;
        paint_text(
            month,
            origin,
            (x, TOP / 2.0 - 1.0),
            Align::Left,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }

    if let Some(((r, c), i)) = hovered {
        let cell = layout.cell(r, c);
        paint_tooltip(
            origin,
            (w, h),
            (cell.right(), cell.y + cell.h / 2.0),
            &chart.tooltip(i),
            &[],
            theme,
            window,
            cx,
        );
    }

    hover.track(hitbox, origin, window, move |x, y| layout.cell_at(x, y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use tbt_core::Micros;
    use tbt_store::TokenSums;

    fn bucket(day: Date, usd_cents: i64, events: u64) -> CostBucket {
        CostBucket {
            start: day.to_zoned_datetime(TimeZone::UTC).unwrap().timestamp(),
            key: "claude_code".to_owned(),
            cost: Micros(usd_cents * 10_000),
            tokens: TokenSums {
                input: events * 100,
                ..TokenSums::default()
            },
            events,
        }
    }

    #[test]
    fn year_starts_on_a_monday_and_ends_on_the_last_day() {
        let last = date(2026, 11, 25);
        let (first, len) = HeatmapChart::days_ending(last);
        assert_eq!(first, date(2025, 11, 24));
        assert_eq!(first.weekday(), jiff::civil::Weekday::Monday);
        assert_eq!(len, 367);
        let chart = HeatmapChart::from_buckets(&[], last, &TimeZone::UTC);
        assert_eq!(chart.weeks(), 53);
        assert_eq!(chart.date(len - 1), Some(last));
        assert_eq!(chart.day_index(2, 52), Some(366));
        assert_eq!(chart.day_index(3, 52), None);
        assert_eq!(chart.day_index(7, 0), None);

        let sunday = date(2026, 11, 29);
        let (first, len) = HeatmapChart::days_ending(sunday);
        assert_eq!(first, date(2025, 11, 24));
        assert_eq!(len, 53 * 7);
    }

    #[test]
    fn range_covers_every_shown_day() {
        let last = date(2026, 11, 25);
        let r = HeatmapChart::range_ending(last, &TimeZone::UTC).unwrap();
        assert_eq!(r.start.to_string(), "2025-11-24T00:00:00Z");
        assert_eq!(r.end.to_string(), "2026-11-26T00:00:00Z");
    }

    #[test]
    fn buckets_sum_into_their_day() {
        let last = date(2026, 11, 25);
        let chart = HeatmapChart::from_buckets(
            &[
                bucket(date(2025, 11, 24), 100, 2),
                bucket(date(2026, 11, 24), 123, 12),
                bucket(date(2026, 11, 24), 7, 3),
                bucket(date(2025, 11, 23), 999, 9),
                bucket(date(2026, 11, 26), 999, 9),
            ],
            last,
            &TimeZone::UTC,
        );
        assert_eq!(chart.cost[0], 1.0);
        assert!((chart.cost[365] - 1.30).abs() < 1e-9);
        assert_eq!(chart.calls[365], 15);
        assert_eq!(chart.calls.iter().sum::<u64>(), 17);
        assert_eq!(chart.value(HeatMetric::Calls, 365), 15.0);
        assert_eq!(chart.value(HeatMetric::Tokens, 365), 1500.0);
        assert_eq!(chart.value(HeatMetric::Cost, 9999), 0.0);
        assert!(!chart.is_empty());
        assert!(HeatmapChart::from_buckets(&[], last, &TimeZone::UTC).is_empty());
    }

    #[test]
    fn days_follow_the_local_time_zone() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let late = CostBucket {
            start: "2026-11-25T03:00:00Z".parse().unwrap(),
            ..bucket(date(2026, 11, 25), 50, 1)
        };
        let chart = HeatmapChart::from_buckets(&[late], date(2026, 11, 25), &tz);
        assert_eq!(chart.calls[365], 1);
    }

    #[test]
    fn square_cells_and_hit_testing() {
        let l = HeatLayout::new(LEFT + 53.0 * 18.0 + RIGHT, 53);
        assert_eq!(l.step, 18.0);
        assert_eq!((l.first_week, l.weeks), (0, 53));
        let c = l.cell(0, 0);
        assert_eq!(c.w, c.h);
        assert_eq!(l.cell_at(LEFT, TOP), Some((0, 0)));
        assert_eq!(l.cell_at(LEFT + 18.0, TOP + 18.0), Some((1, 1)));
        assert_eq!(
            l.cell_at(LEFT + 53.0 * 18.0 - 0.1, TOP + 7.0 * 18.0 - 0.1),
            Some((6, 52))
        );
        assert_eq!(l.cell_at(LEFT + 53.0 * 18.0, TOP), None);
        assert_eq!(l.cell_at(LEFT - 0.1, TOP), None);
        assert_eq!(l.cell_at(LEFT, TOP + 7.0 * 18.0), None);

        let wide = HeatLayout::new(3000.0, 53);
        assert_eq!(wide.step, MAX_STEP);
    }

    #[test]
    fn narrow_layout_keeps_the_latest_weeks() {
        let l = HeatLayout::new(LEFT + 20.0 * MIN_STEP + RIGHT, 53);
        assert_eq!(l.weeks, 20);
        assert_eq!(l.first_week, 33);
        assert_eq!(l.cell_at(LEFT, TOP), Some((0, 33)));
        assert_eq!(l.column_x(33), LEFT);
    }

    #[test]
    fn month_labels_mark_each_new_month() {
        let chart = HeatmapChart::from_buckets(&[], date(2026, 11, 25), &TimeZone::UTC);
        let labels = chart.month_labels(0);
        assert_eq!(labels.first(), Some(&(1, "Dec")));
        assert_eq!(labels.last().map(|l| l.1), Some("Nov"));
        assert_eq!(labels.len(), 12);
        let tail = chart.month_labels(50);
        assert_eq!(tail, vec![(50, "Nov")]);
    }

    #[test]
    fn intensity_is_sqrt_scaled() {
        assert_eq!(intensity(0.0, 10.0), 0.0);
        assert_eq!(intensity(10.0, 10.0), 1.0);
        assert_eq!(intensity(2.5, 10.0), 0.5);
        assert_eq!(intensity(5.0, 0.0), 0.0);
        assert_eq!(intensity(f64::NAN, 10.0), 0.0);
        assert_eq!(intensity(40.0, 10.0), 1.0);
    }

    #[test]
    fn scale_ignores_zero_cells() {
        let s = HeatScale::of([0.0, 0.0, 4.0, 1.0]);
        assert_eq!(
            s,
            HeatScale {
                max: 4.0,
                clamped: false
            }
        );
        assert_eq!(s.intensity(0.0), 0.0);
        assert_eq!(s.intensity(1.0), 0.5);
        assert_eq!(HeatScale::of([0.0; 5]).max, 0.0);
        assert_eq!(HeatScale::of([]).intensity(3.0), 0.0);
    }

    #[test]
    fn scale_clamps_a_single_outlier() {
        let mut values = vec![1.0; 20];
        values.push(100.0);
        let s = HeatScale::of(values.iter().copied());
        assert!(s.clamped);
        assert_eq!(s.max, 1.0);
        assert_eq!(s.intensity(100.0), 1.0);
        assert_eq!(s.intensity(1.0), 1.0);

        let even: Vec<f64> = (1..=20).map(f64::from).collect();
        let s = HeatScale::of(even);
        assert!(!s.clamped);
        assert_eq!(s.max, 20.0);

        let few = HeatScale::of([1.0, 1.0, 100.0]);
        assert!(!few.clamped);
        assert_eq!(few.max, 100.0);
    }

    #[test]
    fn tooltip_text() {
        let last = date(2026, 11, 25);
        let chart = HeatmapChart::from_buckets(
            &[bucket(date(2026, 11, 24), 123, 12), bucket(last, 0, 1)],
            last,
            &TimeZone::UTC,
        );
        assert_eq!(
            chart.tooltip(365),
            "Tue Nov 24, 2026 · $1.23 · 1.2k tok · 12 calls"
        );
        assert_eq!(
            chart.tooltip(366),
            "Wed Nov 25, 2026 · $0.00 · 100 tok · 1 call"
        );
        assert_eq!(
            chart.tooltip(0),
            "Mon Nov 24, 2025 · $0.00 · 0 tok · 0 calls"
        );
    }

    #[test]
    fn metric_formatting() {
        assert_eq!(HeatMetric::Cost.fmt(1.234), "$1.23");
        assert_eq!(HeatMetric::Calls.fmt(12.0), "12");
    }
}
