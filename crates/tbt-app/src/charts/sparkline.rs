use gpui::{Hsla, IntoElement, PathBuilder, Styled, canvas, point, px};

use super::paint::bounds_size;
use super::scale::LinearScale;
use super::stack::max_value;

pub fn sparkline_points(values: &[f64], width: f32, height: f32) -> Vec<(f32, f32)> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let max = max_value(values);
    let y = LinearScale::new(
        (0.0, if max > 0.0 { max } else { 1.0 }),
        (height - 1.0, 1.0),
    );
    let dx = if n > 1 { width / (n - 1) as f32 } else { 0.0 };
    values
        .iter()
        .enumerate()
        .map(|(i, v)| (dx * i as f32, y.map(v.max(0.0))))
        .collect()
}

pub fn sparkline(values: Vec<f64>, color: Hsla) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _cx| {
            let (w, h) = bounds_size(bounds);
            let pts = sparkline_points(&values, w, h);
            if pts.len() < 2 {
                return;
            }
            let o = bounds.origin;
            let at = |(x, y): (f32, f32)| point(o.x + px(x), o.y + px(y));
            let mut area = PathBuilder::fill();
            area.move_to(at((pts[0].0, h)));
            for p in &pts {
                area.line_to(at(*p));
            }
            area.line_to(at((pts[pts.len() - 1].0, h)));
            area.close();
            if let Ok(path) = area.build() {
                window.paint_path(path, color.opacity(0.2));
            }
            let mut line = PathBuilder::stroke(px(1.5));
            line.move_to(at(pts[0]));
            for p in &pts[1..] {
                line.line_to(at(*p));
            }
            if let Ok(path) = line.build() {
                window.paint_path(path, color);
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_span_the_box() {
        let p = sparkline_points(&[0.0, 5.0, 10.0], 100.0, 21.0);
        assert_eq!(p, vec![(0.0, 20.0), (50.0, 10.5), (100.0, 1.0)]);
        assert!(sparkline_points(&[], 100.0, 20.0).is_empty());
        assert_eq!(sparkline_points(&[0.0, 0.0], 10.0, 21.0)[1], (10.0, 20.0));
    }
}
