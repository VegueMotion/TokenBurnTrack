use gpui::{
    App, Bounds, Hitbox, IntoElement, PathBuilder, Pixels, Point, SharedString, Styled, Window,
    canvas, fill, point, px, size,
};

use super::Series;
use super::format::ValueKind;
use super::paint::{
    ChartTheme, Hover, TooltipRow, bounds_size, insert_hitbox, paint_category_axis, paint_rect,
    paint_tooltip, paint_value_axis,
};
use super::scale::{CartesianLayout, Margins, Rect};
use super::stack::{max_value, stack, totals};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMode {
    Line,
    Area,
    StackedArea,
}

#[derive(Clone)]
pub struct LineChart {
    pub labels: Vec<SharedString>,
    pub titles: Vec<SharedString>,
    pub series: Vec<Series>,
    pub kind: ValueKind,
    pub mode: LineMode,
}

pub fn line_chart(chart: LineChart, hover: Hover, theme: ChartTheme) -> impl IntoElement + Styled {
    canvas(
        |bounds, window, _cx| insert_hitbox(bounds, window),
        move |bounds, hitbox, window, cx| {
            paint(&chart, &hover, &theme, bounds, &hitbox, window, cx)
        },
    )
}

pub fn line_bands(chart: &LineChart) -> Vec<Vec<(f64, f64)>> {
    let values: Vec<Vec<f64>> = chart.series.iter().map(|s| s.values.clone()).collect();
    match chart.mode {
        LineMode::StackedArea => stack(&values),
        LineMode::Line | LineMode::Area => values
            .iter()
            .map(|v| v.iter().map(|y| (0.0, y.max(0.0))).collect())
            .collect(),
    }
}

fn abs(origin: Point<Pixels>, x: f32, y: f32) -> Point<Pixels> {
    point(origin.x + px(x), origin.y + px(y))
}

#[allow(clippy::too_many_arguments)]
fn paint(
    chart: &LineChart,
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
    let bands = line_bands(chart);
    let max = bands
        .iter()
        .map(|b| max_value(&b.iter().map(|(_, y1)| *y1).collect::<Vec<_>>()))
        .fold(0.0, f64::max);
    let layout = CartesianLayout::new(w, h, n, max, Margins::AXES, 0.0);
    let hovered = hover.get().map(|(_, i)| i).filter(|i| *i < n);

    paint_value_axis(origin, &layout, chart.kind, theme, window, cx);

    for (si, band) in bands.iter().enumerate() {
        let color = chart.series[si].color;
        let len = band.len().min(n);
        if len == 0 {
            continue;
        }
        let x_of = |i: usize| layout.x.center(i);
        if chart.mode != LineMode::Line && len > 1 {
            let mut area = PathBuilder::fill();
            area.move_to(abs(origin, x_of(0), layout.y.map(band[0].1)));
            for (i, (_, y1)) in band.iter().enumerate().take(len).skip(1) {
                area.line_to(abs(origin, x_of(i), layout.y.map(*y1)));
            }
            for i in (0..len).rev() {
                area.line_to(abs(origin, x_of(i), layout.y.map(band[i].0)));
            }
            area.close();
            let alpha = if chart.mode == LineMode::StackedArea {
                0.55
            } else {
                0.25
            };
            if let Ok(path) = area.build() {
                window.paint_path(path, color.opacity(alpha));
            }
        }
        let mut line = PathBuilder::stroke(px(2.));
        line.move_to(abs(origin, x_of(0), layout.y.map(band[0].1)));
        for (i, (_, y1)) in band.iter().enumerate().take(len).skip(1) {
            line.line_to(abs(origin, x_of(i), layout.y.map(*y1)));
        }
        if len > 1
            && let Ok(path) = line.build()
        {
            window.paint_path(path, color);
        }
    }

    paint_category_axis(origin, &layout, &chart.labels, theme, window, cx);

    if let Some(i) = hovered {
        let cx_ = layout.x.center(i);
        paint_rect(
            origin,
            Rect {
                x: cx_.round(),
                y: layout.plot.y,
                w: 1.0,
                h: layout.plot.h,
            },
            theme.muted.opacity(0.6),
            window,
        );
        for (si, band) in bands.iter().enumerate() {
            if let Some((_, y1)) = band.get(i) {
                let y = layout.y.map(*y1);
                let r = 4.0;
                window.paint_quad(
                    fill(
                        gpui::Bounds::new(
                            abs(origin, cx_ - r, y - r),
                            size(px(r * 2.0), px(r * 2.0)),
                        ),
                        chart.series[si].color,
                    )
                    .corner_radii(px(r))
                    .border_widths(px(2.))
                    .border_color(theme.surface),
                );
            }
        }
        let mut rows: Vec<TooltipRow> = chart
            .series
            .iter()
            .rev()
            .map(|s| TooltipRow {
                swatch: (chart.series.len() > 1).then_some(s.color),
                label: s.name.clone(),
                value: chart
                    .kind
                    .fmt(s.values.get(i).copied().unwrap_or(0.0))
                    .into(),
            })
            .collect();
        if chart.mode == LineMode::StackedArea && chart.series.len() > 1 {
            let values: Vec<Vec<f64>> = chart.series.iter().map(|s| s.values.clone()).collect();
            rows.push(TooltipRow {
                swatch: None,
                label: "Total".into(),
                value: chart
                    .kind
                    .fmt(totals(&values).get(i).copied().unwrap_or(0.0))
                    .into(),
            });
        }
        let title = chart.titles.get(i).or(chart.labels.get(i));
        paint_tooltip(
            origin,
            (w, h),
            (cx_, layout.plot.y + layout.plot.h / 2.0),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chart(mode: LineMode) -> LineChart {
        let s = |v: Vec<f64>| Series {
            name: "s".into(),
            color: gpui::black(),
            values: v,
        };
        LineChart {
            labels: vec!["a".into(), "b".into()],
            titles: vec![],
            series: vec![s(vec![1.0, 2.0]), s(vec![3.0, -1.0])],
            kind: ValueKind::Tokens,
            mode,
        }
    }

    #[test]
    fn stacked_area_bands_accumulate() {
        let b = line_bands(&chart(LineMode::StackedArea));
        assert_eq!(b[1], vec![(1.0, 4.0), (2.0, 2.0)]);
        let b = line_bands(&chart(LineMode::Line));
        assert_eq!(b[1], vec![(0.0, 3.0), (0.0, 0.0)]);
    }
}
