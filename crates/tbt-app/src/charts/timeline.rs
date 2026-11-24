use gpui::{
    App, Bounds, Hitbox, Hsla, IntoElement, PathBuilder, Pixels, SharedString, Styled, Window,
    canvas, fill, point, px, size,
};

use super::Series;
use super::format::{ValueKind, fmt_usd_tick};
use super::paint::{
    Align, ChartTheme, Hover, LABEL_SIZE, TooltipRow, bounds_size, insert_hitbox, paint_bar,
    paint_category_axis, paint_rect, paint_text, paint_tooltip, paint_value_axis,
};
use super::scale::{CartesianLayout, LinearScale, Margins, Rect, nice_step};
use super::stack::{max_value, stack, totals};

pub const MARGINS: Margins = Margins {
    left: 56.0,
    right: 60.0,
    top: 10.0,
    bottom: 24.0,
};

#[derive(Clone, Debug)]
pub struct TipRow {
    pub swatch: Option<Hsla>,
    pub label: SharedString,
    pub value: SharedString,
}

impl TipRow {
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> TipRow {
        TipRow {
            swatch: None,
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone)]
pub struct CallTimeline {
    pub labels: Vec<SharedString>,
    pub series: Vec<Series>,
    pub cumulative: Series,
    pub titles: Vec<SharedString>,
    pub tooltips: Vec<Vec<TipRow>>,
}

pub fn bar_padding(count: usize) -> f32 {
    if count > 120 { 0.0 } else { 0.2 }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineLayout {
    pub bars: CartesianLayout,
    pub cost_y: LinearScale,
    pub cost_step: f64,
    pub cost_ticks: Vec<f64>,
}

impl TimelineLayout {
    pub fn new(width: f32, height: f32, count: usize, max_tokens: f64, max_cost: f64) -> Self {
        let bars = CartesianLayout::new(
            width,
            height,
            count,
            max_tokens,
            MARGINS,
            bar_padding(count),
        );
        let intervals = bars.ticks.values.len().saturating_sub(1).max(1);
        let cost_step = nice_step(max_cost.max(0.0), intervals);
        let top = cost_step * intervals as f64;
        TimelineLayout {
            cost_y: LinearScale::new((0.0, top), (bars.plot.bottom(), bars.plot.y)),
            cost_ticks: (0..=intervals).map(|i| cost_step * i as f64).collect(),
            cost_step,
            bars,
        }
    }
}

pub fn call_timeline(
    chart: CallTimeline,
    hover: Hover,
    theme: ChartTheme,
) -> impl IntoElement + Styled {
    canvas(
        |bounds, window, _cx| insert_hitbox(bounds, window),
        move |bounds, hitbox, window, cx| {
            paint(&chart, &hover, &theme, bounds, &hitbox, window, cx)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn paint(
    chart: &CallTimeline,
    hover: &Hover,
    theme: &ChartTheme,
    bounds: Bounds<Pixels>,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = bounds.origin;
    let (w, h) = bounds_size(bounds);
    let n = chart.labels.len();
    let values: Vec<Vec<f64>> = chart.series.iter().map(|s| s.values.clone()).collect();
    let stacks = stack(&values);
    let sums = totals(&values);
    let cum = &chart.cumulative.values;
    let layout = TimelineLayout::new(w, h, n, max_value(&sums), max_value(cum));
    let bars = &layout.bars;
    let hovered = hover.get().map(|(_, i)| i).filter(|i| *i < n);

    if let Some(i) = hovered {
        let step = bars.x.step();
        paint_rect(
            origin,
            Rect {
                x: bars.x.start + step * i as f32,
                y: bars.plot.y,
                w: step,
                h: bars.plot.h,
            },
            theme.highlight,
            window,
        );
    }
    paint_value_axis(origin, bars, ValueKind::Tokens, theme, window, cx);
    for v in &layout.cost_ticks {
        paint_text(
            fmt_usd_tick(*v, layout.cost_step),
            origin,
            (bars.plot.right() + 8.0, layout.cost_y.map(*v)),
            Align::Left,
            chart.cumulative.color,
            LABEL_SIZE,
            window,
            cx,
        );
    }

    for i in 0..n {
        let (bx, bw) = bars.x.band(i);
        let top_series = stacks.iter().rposition(|s| s[i].1 > s[i].0);
        for (si, s) in stacks.iter().enumerate() {
            let (y0, y1) = s[i];
            if y1 <= y0 {
                continue;
            }
            let top = bars.y.map(y1);
            let bottom = bars.y.map(y0);
            let r = Rect {
                x: bx,
                y: top,
                w: bw.max(1.0),
                h: (bottom - top).max(1.0),
            };
            let radius = if Some(si) == top_series && bw > 6.0 {
                2.0
            } else {
                0.0
            };
            paint_bar(origin, r, radius, chart.series[si].color, window);
        }
    }

    let point_at = |i: usize| {
        let y = layout.cost_y.map(cum.get(i).copied().unwrap_or(0.0));
        point(origin.x + px(bars.x.center(i)), origin.y + px(y))
    };
    if n > 1 {
        let mut line = PathBuilder::stroke(px(2.));
        line.move_to(point_at(0));
        for i in 1..n {
            line.line_to(point_at(i));
        }
        if let Ok(path) = line.build() {
            window.paint_path(path, chart.cumulative.color);
        }
    }
    paint_category_axis(origin, bars, &chart.labels, theme, window, cx);

    if let Some(i) = hovered.or((n == 1).then_some(0)) {
        let p = point_at(i);
        let r = 4.0;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(p.x - px(r), p.y - px(r)),
                    size(px(r * 2.), px(r * 2.)),
                ),
                chart.cumulative.color,
            )
            .corner_radii(px(r))
            .border_widths(px(2.))
            .border_color(theme.surface),
        );
    }

    if let Some(i) = hovered {
        let rows: Vec<TooltipRow> = chart
            .tooltips
            .get(i)
            .into_iter()
            .flatten()
            .map(|r| TooltipRow {
                swatch: r.swatch,
                label: r.label.clone(),
                value: r.value.clone(),
            })
            .collect();
        let anchor_y = bars
            .y
            .map(sums.get(i).copied().unwrap_or(0.0))
            .min(f32::from(point_at(i).y - origin.y));
        paint_tooltip(
            origin,
            (w, h),
            (bars.x.center(i), anchor_y.min(bars.plot.bottom() - 20.0)),
            chart.titles.get(i).map(|t| t.as_ref()).unwrap_or(""),
            &rows,
            theme,
            window,
            cx,
        );
    }

    let hit_layout = bars.clone();
    hover.track(hitbox, origin, window, move |x, y| {
        hit_layout.index_at(x, y).map(|i| (0, i))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_axis_shares_gridlines_with_token_axis() {
        let l = TimelineLayout::new(600.0, 240.0, 40, 180_000.0, 2.7);
        let token_ticks = &l.bars.ticks.values;
        assert_eq!(l.cost_ticks.len(), token_ticks.len());
        for (c, t) in l.cost_ticks.iter().zip(token_ticks) {
            assert!((l.cost_y.map(*c) - l.bars.y.map(*t)).abs() < 1e-3);
        }
        assert!(*l.cost_ticks.last().unwrap() >= 2.7);
        assert!(l.cost_y.map(2.7) >= l.bars.plot.y);
    }

    #[test]
    fn empty_cost_still_has_an_axis() {
        let l = TimelineLayout::new(600.0, 240.0, 3, 0.0, 0.0);
        assert!(l.cost_step > 0.0);
        assert_eq!(l.cost_y.map(0.0), l.bars.plot.bottom());
    }

    #[test]
    fn many_calls_drop_bar_padding() {
        assert_eq!(bar_padding(10), 0.2);
        assert_eq!(bar_padding(500), 0.0);
        let l = TimelineLayout::new(600.0, 240.0, 500, 1.0, 1.0);
        assert_eq!(l.bars.x.index_at(l.bars.plot.right() - 0.1), Some(499));
    }
}
