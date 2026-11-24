use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Hitbox, Hsla, IntoElement, ParentElement, Pixels, SharedString,
    Styled, Window, canvas, div, fill, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, Sizable, h_flex, v_flex};
use tbt_store::HeatCell;

use super::format::{fmt_count, fmt_usd, micros_to_usd};
use super::paint::{
    Align, ChartTheme, Hover, LABEL_SIZE, bounds_size, insert_hitbox, paint_text, paint_tooltip,
    rect_bounds,
};
use super::scale::{BandScale, Margins, Rect};
use crate::palette::heat_color;

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HOURS: usize = 24;
const OUTLIER_MIN_CELLS: usize = 8;
const OUTLIER_QUANTILE: f64 = 0.95;
const OUTLIER_RATIO: f64 = 2.0;
const LEGEND_STEPS: usize = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeatMetric {
    #[default]
    Cost,
    Calls,
}

impl HeatMetric {
    pub const ALL: [HeatMetric; 2] = [HeatMetric::Cost, HeatMetric::Calls];

    pub fn label(self) -> &'static str {
        match self {
            HeatMetric::Cost => "Cost",
            HeatMetric::Calls => "Calls",
        }
    }

    pub fn fmt(self, v: f64) -> String {
        match self {
            HeatMetric::Cost => fmt_usd(v),
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
    pub cost: [[f64; HOURS]; 7],
    pub calls: [[u64; HOURS]; 7],
    pub time_zone: String,
}

impl HeatmapChart {
    pub fn from_cells(cells: &[HeatCell], time_zone: impl Into<String>) -> HeatmapChart {
        let mut chart = HeatmapChart {
            cost: [[0.0; HOURS]; 7],
            calls: [[0; HOURS]; 7],
            time_zone: time_zone.into(),
        };
        for c in cells {
            let (r, h) = (c.weekday as usize, c.hour as usize);
            if r < 7 && h < HOURS {
                chart.cost[r][h] += micros_to_usd(c.cost);
                chart.calls[r][h] += c.events;
            }
        }
        chart
    }

    pub fn is_empty(&self) -> bool {
        self.calls.iter().flatten().all(|c| *c == 0)
            && self.cost.iter().flatten().all(|c| *c <= 0.0)
    }

    pub fn value(&self, metric: HeatMetric, weekday: usize, hour: usize) -> f64 {
        if weekday >= 7 || hour >= HOURS {
            return 0.0;
        }
        match metric {
            HeatMetric::Cost => self.cost[weekday][hour],
            HeatMetric::Calls => self.calls[weekday][hour] as f64,
        }
    }

    pub fn scale(&self, metric: HeatMetric) -> HeatScale {
        HeatScale::of(
            (0..7)
                .flat_map(|r| (0..HOURS).map(move |h| (r, h)))
                .map(|(r, h)| self.value(metric, r, h)),
        )
    }

    pub fn tooltip(&self, weekday: usize, hour: usize) -> String {
        let calls = self
            .calls
            .get(weekday)
            .and_then(|r| r.get(hour))
            .copied()
            .unwrap_or(0);
        format!(
            "{} {:02}:00–{:02}:00 · {} · {} {}",
            WEEKDAYS.get(weekday).copied().unwrap_or("?"),
            hour,
            hour + 1,
            fmt_usd(self.value(HeatMetric::Cost, weekday, hour)),
            fmt_count(calls),
            if calls == 1 { "call" } else { "calls" }
        )
    }

    pub fn hour_label_stride(cell_step: f32, label_width: f32) -> usize {
        if cell_step <= 0.0 {
            return HOURS;
        }
        let groups = (label_width / (3.0 * cell_step)).ceil().max(1.0) as usize;
        (3 * groups).min(HOURS)
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
                .h(px(220.))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("No activity in this range")
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
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Local time · {}", self.time_zone)),
                    )
                    .child(toggle),
            )
            .child(
                heatmap_chart(self.clone(), current, hover, theme)
                    .w_full()
                    .h(px(200.)),
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

fn empty_color(theme: &ChartTheme) -> Hsla {
    theme.grid.opacity(0.5)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeatLayout {
    pub plot: Rect,
    pub rows: BandScale,
    pub cols: BandScale,
}

const MARGINS: Margins = Margins {
    left: 40.0,
    right: 8.0,
    top: 4.0,
    bottom: 22.0,
};

impl HeatLayout {
    pub fn new(width: f32, height: f32, rows: usize, cols: usize) -> Self {
        let plot = MARGINS.plot(width, height);
        HeatLayout {
            plot,
            rows: BandScale::new(rows, plot.y, plot.bottom(), 0.1),
            cols: BandScale::new(cols, plot.x, plot.right(), 0.1),
        }
    }

    pub fn cell_at(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        Some((self.rows.index_at(y)?, self.cols.index_at(x)?))
    }

    pub fn cell(&self, row: usize, col: usize) -> Rect {
        let (x, w) = self.cols.band(col);
        let (y, h) = self.rows.band(row);
        Rect { x, y, w, h }
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
    let layout = HeatLayout::new(w, h, WEEKDAYS.len(), HOURS);
    let scale = chart.scale(metric);
    let hovered = hover.get().filter(|(r, c)| *r < 7 && *c < HOURS);

    for (r, day) in WEEKDAYS.iter().enumerate() {
        for c in 0..HOURS {
            let v = chart.value(metric, r, c);
            let color = if v > 0.0 {
                heat_color(scale.intensity(v))
            } else {
                empty_color(theme)
            };
            let mut q = fill(rect_bounds(origin, layout.cell(r, c)), color).corner_radii(px(2.));
            if hovered == Some((r, c)) {
                q = q.border_widths(px(1.5)).border_color(theme.foreground);
            }
            window.paint_quad(q);
        }
        paint_text(
            *day,
            origin,
            (layout.plot.x - 8.0, layout.rows.center(r)),
            Align::Right,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }
    let stride = HeatmapChart::hour_label_stride(layout.cols.step(), 34.0);
    for c in (0..HOURS).step_by(stride) {
        let (x, _) = layout.cols.band(c);
        paint_text(
            format!("{c:02}:00"),
            origin,
            (x, layout.plot.bottom() + 11.0),
            Align::Left,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }

    if let Some((r, c)) = hovered {
        let cell = layout.cell(r, c);
        paint_tooltip(
            origin,
            (w, h),
            (cell.right(), cell.y + cell.h / 2.0),
            &chart.tooltip(r, c),
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
    use tbt_core::Micros;

    fn cell(weekday: u8, hour: u8, usd_cents: i64, events: u64) -> HeatCell {
        HeatCell {
            weekday,
            hour,
            cost: Micros(usd_cents * 10_000),
            events,
        }
    }

    #[test]
    fn cell_hit_testing() {
        let l = HeatLayout::new(40.0 + 240.0 + 8.0, 4.0 + 70.0 + 22.0, 7, 24);
        assert_eq!(l.cols.step(), 10.0);
        assert_eq!(l.rows.step(), 10.0);
        assert_eq!(l.cell_at(40.0, 4.0), Some((0, 0)));
        assert_eq!(l.cell_at(49.999, 13.999), Some((0, 0)));
        assert_eq!(l.cell_at(50.0, 14.0), Some((1, 1)));
        assert_eq!(l.cell_at(279.9, 73.9), Some((6, 23)));
        assert_eq!(l.cell_at(280.0, 20.0), None);
        assert_eq!(l.cell_at(39.9, 20.0), None);
        assert_eq!(l.cell_at(100.0, 74.0), None);
    }

    #[test]
    fn cells_map_monday_first_and_ignore_out_of_range() {
        let chart = HeatmapChart::from_cells(
            &[
                cell(0, 0, 100, 2),
                cell(1, 14, 123, 12),
                cell(6, 23, 50, 1),
                cell(1, 14, 7, 3),
                cell(7, 3, 999, 9),
                cell(2, 24, 999, 9),
            ],
            "Europe/Berlin",
        );
        assert_eq!(chart.cost[0][0], 1.0);
        assert!((chart.cost[1][14] - 1.30).abs() < 1e-9);
        assert_eq!(chart.calls[1][14], 15);
        assert_eq!(chart.calls[6][23], 1);
        assert_eq!(chart.calls.iter().flatten().sum::<u64>(), 18);
        assert_eq!(chart.value(HeatMetric::Calls, 6, 23), 1.0);
        assert_eq!(chart.value(HeatMetric::Cost, 7, 0), 0.0);
        assert!(!chart.is_empty());
        assert!(HeatmapChart::from_cells(&[], "UTC").is_empty());
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
        let chart = HeatmapChart::from_cells(&[cell(1, 14, 123, 12), cell(6, 23, 0, 1)], "UTC");
        assert_eq!(chart.tooltip(1, 14), "Tue 14:00–15:00 · $1.23 · 12 calls");
        assert_eq!(chart.tooltip(6, 23), "Sun 23:00–24:00 · $0.00 · 1 call");
        assert_eq!(chart.tooltip(0, 0), "Mon 00:00–01:00 · $0.00 · 0 calls");
    }

    #[test]
    fn hour_labels_every_three_hours_or_more() {
        assert_eq!(HeatmapChart::hour_label_stride(20.0, 34.0), 3);
        assert_eq!(HeatmapChart::hour_label_stride(12.0, 34.0), 3);
        assert_eq!(HeatmapChart::hour_label_stride(8.0, 34.0), 6);
        assert_eq!(HeatmapChart::hour_label_stride(0.0, 34.0), 24);
    }

    #[test]
    fn metric_formatting() {
        assert_eq!(HeatMetric::Cost.fmt(1.234), "$1.23");
        assert_eq!(HeatMetric::Calls.fmt(12.0), "12");
    }
}
