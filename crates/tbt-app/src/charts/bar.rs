use gpui::{App, Bounds, Hitbox, IntoElement, Pixels, Styled, Window, canvas};

use super::Series;
use super::format::ValueKind;
use super::paint::{
    ChartTheme, Hover, TooltipRow, bounds_size, insert_hitbox, paint_bar, paint_category_axis,
    paint_rect, paint_tooltip, paint_value_axis,
};
use super::scale::{CartesianLayout, Margins, Rect};
use super::stack::{max_value, stack, totals};

const SEGMENT_GAP: f32 = 2.0;

#[derive(Clone)]
pub struct BarChart {
    pub labels: Vec<gpui::SharedString>,
    pub titles: Vec<gpui::SharedString>,
    pub series: Vec<Series>,
    pub kind: ValueKind,
}

pub fn bar_chart(chart: BarChart, hover: Hover, theme: ChartTheme) -> impl IntoElement + Styled {
    canvas(
        |bounds, window, _cx| insert_hitbox(bounds, window),
        move |bounds, hitbox, window, cx| {
            paint(&chart, &hover, &theme, bounds, &hitbox, window, cx)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn paint(
    chart: &BarChart,
    hover: &Hover,
    theme: &ChartTheme,
    bounds: Bounds<Pixels>,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = bounds.origin;
    let (w, h) = bounds_size(bounds);
    let values: Vec<Vec<f64>> = chart.series.iter().map(|s| s.values.clone()).collect();
    let stacks = stack(&values);
    let sums = totals(&values);
    let n = chart.labels.len();
    let layout = CartesianLayout::new(w, h, n, max_value(&sums), Margins::AXES, 0.3);
    let hovered = hover.get().map(|(_, i)| i).filter(|i| *i < n);

    if let Some(i) = hovered {
        let step = layout.x.step();
        paint_rect(
            origin,
            Rect {
                x: layout.x.start + step * i as f32,
                y: layout.plot.y,
                w: step,
                h: layout.plot.h,
            },
            theme.highlight,
            window,
        );
    }
    paint_value_axis(origin, &layout, chart.kind, theme, window, cx);

    for i in 0..n {
        let (bx, bw) = layout.x.band(i);
        let top_series = stacks.iter().rposition(|s| s[i].1 > s[i].0);
        for (si, s) in stacks.iter().enumerate() {
            let (y0, y1) = s[i];
            if y1 <= y0 {
                continue;
            }
            let top = layout.y.map(y1);
            let mut bottom = layout.y.map(y0);
            if y0 > 0.0 && bottom - top > SEGMENT_GAP {
                bottom -= SEGMENT_GAP;
            }
            let r = Rect {
                x: bx,
                y: top,
                w: bw,
                h: (bottom - top).max(1.0),
            };
            let radius = if Some(si) == top_series { 3.0 } else { 0.0 };
            paint_bar(origin, r, radius, chart.series[si].color, window);
        }
    }
    paint_category_axis(origin, &layout, &chart.labels, theme, window, cx);

    if let Some(i) = hovered {
        let mut rows: Vec<TooltipRow> = chart
            .series
            .iter()
            .rev()
            .filter(|s| chart.series.len() == 1 || s.values.get(i).copied().unwrap_or(0.0) > 0.0)
            .map(|s| TooltipRow {
                swatch: (chart.series.len() > 1).then_some(s.color),
                label: s.name.clone(),
                value: chart
                    .kind
                    .fmt(s.values.get(i).copied().unwrap_or(0.0))
                    .into(),
            })
            .collect();
        if chart.series.len() > 1 {
            rows.push(TooltipRow {
                swatch: None,
                label: "Total".into(),
                value: chart.kind.fmt(sums.get(i).copied().unwrap_or(0.0)).into(),
            });
        }
        let title = chart.titles.get(i).or(chart.labels.get(i));
        let anchor_y = layout.y.map(sums.get(i).copied().unwrap_or(0.0));
        paint_tooltip(
            origin,
            (w, h),
            (
                layout.x.center(i),
                anchor_y.min(layout.plot.bottom() - 20.0),
            ),
            title.map(|t| t.as_ref()).unwrap_or(""),
            &rows,
            theme,
            window,
            cx,
        );
    }

    let hit_layout = layout.clone();
    hover.track(hitbox, origin, window, move |x, y| {
        hit_layout.index_at(x, y).map(|i| (0, i))
    });
}
