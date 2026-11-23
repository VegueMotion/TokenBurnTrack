use gpui::{App, Bounds, Hitbox, IntoElement, Pixels, SharedString, Styled, Window, canvas, fill};

use super::format::ValueKind;
use super::paint::{
    Align, ChartTheme, Hover, LABEL_SIZE, TooltipRow, bounds_size, insert_hitbox, paint_text,
    paint_tooltip, rect_bounds,
};
use super::scale::{BandScale, Margins, Rect, label_stride};
use crate::palette::heat_color;

#[derive(Clone)]
pub struct HeatMetric {
    pub name: SharedString,
    pub kind: ValueKind,
    pub values: Vec<Vec<f64>>,
}

#[derive(Clone)]
pub struct HeatmapChart {
    pub row_labels: Vec<SharedString>,
    pub col_labels: Vec<SharedString>,
    pub metrics: Vec<HeatMetric>,
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

pub fn intensity(v: f64, max: f64) -> f32 {
    if max <= 0.0 || v <= 0.0 {
        0.0
    } else {
        (v / max).sqrt().clamp(0.0, 1.0) as f32
    }
}

pub fn heatmap_chart(
    chart: HeatmapChart,
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
    chart: &HeatmapChart,
    hover: &Hover,
    theme: &ChartTheme,
    bounds: Bounds<Pixels>,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = bounds.origin;
    let (w, h) = bounds_size(bounds);
    let (nr, nc) = (chart.row_labels.len(), chart.col_labels.len());
    let layout = HeatLayout::new(w, h, nr, nc);
    let Some(primary) = chart.metrics.first() else {
        return;
    };
    let value = |r: usize, c: usize| {
        primary
            .values
            .get(r)
            .and_then(|row| row.get(c))
            .copied()
            .unwrap_or(0.0)
    };
    let max = primary.values.iter().flatten().copied().fold(0.0, f64::max);
    let hovered = hover.get().filter(|(r, c)| *r < nr && *c < nc);

    for (r, row_label) in chart.row_labels.iter().enumerate() {
        for c in 0..nc {
            let v = value(r, c);
            let color = if v > 0.0 {
                heat_color(intensity(v, max))
            } else {
                theme.grid.opacity(0.5)
            };
            let mut q =
                fill(rect_bounds(origin, layout.cell(r, c)), color).corner_radii(gpui::px(2.));
            if hovered == Some((r, c)) {
                q = q
                    .border_widths(gpui::px(1.5))
                    .border_color(theme.foreground);
            }
            window.paint_quad(q);
        }
        paint_text(
            row_label.clone(),
            origin,
            (layout.plot.x - 8.0, layout.rows.center(r)),
            Align::Right,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }
    let stride = label_stride(nc, layout.plot.w, 36.0);
    for c in (0..nc).step_by(stride) {
        paint_text(
            chart.col_labels[c].clone(),
            origin,
            (layout.cols.center(c), layout.plot.bottom() + 11.0),
            Align::Center,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }

    if let Some((r, c)) = hovered {
        let rows: Vec<TooltipRow> = chart
            .metrics
            .iter()
            .map(|m| TooltipRow {
                swatch: None,
                label: m.name.clone(),
                value: m
                    .kind
                    .fmt(
                        m.values
                            .get(r)
                            .and_then(|row| row.get(c))
                            .copied()
                            .unwrap_or(0.0),
                    )
                    .into(),
            })
            .collect();
        let cell = layout.cell(r, c);
        paint_tooltip(
            origin,
            (w, h),
            (cell.right(), cell.y + cell.h / 2.0),
            &format!("{} · {}", chart.row_labels[r], chart.col_labels[c]),
            &rows,
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
    fn intensity_is_sqrt_scaled() {
        assert_eq!(intensity(0.0, 10.0), 0.0);
        assert_eq!(intensity(10.0, 10.0), 1.0);
        assert_eq!(intensity(2.5, 10.0), 0.5);
        assert_eq!(intensity(5.0, 0.0), 0.0);
    }
}
