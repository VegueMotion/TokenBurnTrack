#[derive(Debug, Clone, PartialEq)]
pub struct Ticks {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub values: Vec<f64>,
}

pub fn nice_step(span: f64, max_ticks: usize) -> f64 {
    let rough = span / max_ticks.max(1) as f64;
    if !(rough.is_finite() && rough > 0.0) {
        return 1.0;
    }
    let mag = 10f64.powf(rough.log10().floor());
    let norm = rough / mag;
    let nice = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

pub fn nice_ticks(min: f64, max: f64, max_ticks: usize) -> Ticks {
    let (mut lo, mut hi) = if min <= max { (min, max) } else { (max, min) };
    if !(lo.is_finite() && hi.is_finite()) {
        (lo, hi) = (0.0, 1.0);
    }
    if lo == hi {
        if lo == 0.0 {
            hi = 1.0;
        } else if lo > 0.0 {
            lo = 0.0;
        } else {
            hi = 0.0;
        }
    }
    let step = nice_step(hi - lo, max_ticks);
    let first = (lo / step).floor();
    let last = (hi / step).ceil();
    let values: Vec<f64> = (0..=((last - first) as i64))
        .map(|i| clean((first + i as f64) * step, step))
        .collect();
    Ticks {
        min: values[0],
        max: *values.last().unwrap_or(&values[0]),
        step,
        values,
    }
}

fn clean(v: f64, step: f64) -> f64 {
    let decimals = (-step.log10().floor()).max(0.0) as i32 + 1;
    let f = 10f64.powi(decimals);
    let r = (v * f).round() / f;
    if r == 0.0 { 0.0 } else { r }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearScale {
    pub d0: f64,
    pub d1: f64,
    pub r0: f32,
    pub r1: f32,
}

impl LinearScale {
    pub fn new(domain: (f64, f64), range: (f32, f32)) -> Self {
        LinearScale {
            d0: domain.0,
            d1: domain.1,
            r0: range.0,
            r1: range.1,
        }
    }

    pub fn map(&self, v: f64) -> f32 {
        let span = self.d1 - self.d0;
        if span == 0.0 {
            return self.r0;
        }
        let t = ((v - self.d0) / span) as f32;
        self.r0 + t * (self.r1 - self.r0)
    }

    pub fn invert(&self, p: f32) -> f64 {
        let span = self.r1 - self.r0;
        if span == 0.0 {
            return self.d0;
        }
        self.d0 + ((p - self.r0) / span) as f64 * (self.d1 - self.d0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandScale {
    pub count: usize,
    pub start: f32,
    pub end: f32,
    pub padding: f32,
}

impl BandScale {
    pub fn new(count: usize, start: f32, end: f32, padding: f32) -> Self {
        BandScale {
            count,
            start,
            end,
            padding: padding.clamp(0.0, 0.95),
        }
    }

    pub fn step(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            (self.end - self.start) / self.count as f32
        }
    }

    pub fn band(&self, i: usize) -> (f32, f32) {
        let step = self.step();
        let gap = step * self.padding;
        (self.start + step * i as f32 + gap / 2.0, step - gap)
    }

    pub fn center(&self, i: usize) -> f32 {
        self.start + self.step() * (i as f32 + 0.5)
    }

    pub fn index_at(&self, p: f32) -> Option<usize> {
        if self.count == 0 || p < self.start || p >= self.end {
            return None;
        }
        let i = ((p - self.start) / self.step()).floor() as usize;
        Some(i.min(self.count - 1))
    }
}

pub fn label_stride(count: usize, available: f32, min_label_width: f32) -> usize {
    if count == 0 || available <= 0.0 {
        return 1;
    }
    let fits = (available / min_label_width).floor().max(1.0) as usize;
    count.div_ceil(fits).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Margins {
    pub const AXES: Margins = Margins {
        left: 56.0,
        right: 12.0,
        top: 10.0,
        bottom: 24.0,
    };

    pub fn plot(&self, width: f32, height: f32) -> Rect {
        Rect {
            x: self.left,
            y: self.top,
            w: (width - self.left - self.right).max(0.0),
            h: (height - self.top - self.bottom).max(0.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CartesianLayout {
    pub plot: Rect,
    pub x: BandScale,
    pub y: LinearScale,
    pub ticks: Ticks,
}

impl CartesianLayout {
    pub fn new(
        width: f32,
        height: f32,
        count: usize,
        max_value: f64,
        margins: Margins,
        padding: f32,
    ) -> Self {
        let plot = margins.plot(width, height);
        let max_ticks = ((plot.h / 36.0).floor() as usize).clamp(2, 6);
        let ticks = nice_ticks(0.0, max_value.max(0.0), max_ticks);
        CartesianLayout {
            plot,
            x: BandScale::new(count, plot.x, plot.right(), padding),
            y: LinearScale::new((ticks.min, ticks.max), (plot.bottom(), plot.y)),
            ticks,
        }
    }

    pub fn index_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.plot.contains(x, y) {
            return None;
        }
        self.x.index_at(x)
    }
}

pub fn tooltip_origin(anchor: (f32, f32), size: (f32, f32), bounds: (f32, f32)) -> (f32, f32) {
    const GAP: f32 = 12.0;
    let (ax, ay) = anchor;
    let (w, h) = size;
    let (bw, bh) = bounds;
    let x = if ax + GAP + w <= bw {
        ax + GAP
    } else if ax - GAP - w >= 0.0 {
        ax - GAP - w
    } else {
        (bw - w).max(0.0)
    };
    let y = (ay - h / 2.0).clamp(0.0, (bh - h).max(0.0));
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn nice_step_picks_1_2_25_5() {
        assert_eq!(nice_step(87.0, 5), 20.0);
        assert_eq!(nice_step(100.0, 10), 10.0);
        assert_eq!(nice_step(12.0, 5), 2.5);
        assert_eq!(nice_step(0.9, 4), 0.25);
        assert_eq!(nice_step(0.0, 5), 1.0);
    }

    #[test]
    fn nice_ticks_cover_range() {
        let t = nice_ticks(0.0, 87.0, 5);
        assert_eq!(t.step, 20.0);
        assert!(approx(&t.values, &[0.0, 20.0, 40.0, 60.0, 80.0, 100.0]));
        assert_eq!((t.min, t.max), (0.0, 100.0));

        let t = nice_ticks(0.0, 1.0, 5);
        assert!(approx(&t.values, &[0.0, 0.2, 0.4, 0.6, 0.8, 1.0]));
        assert_eq!(t.values[3], 0.6);

        let t = nice_ticks(-3.0, 7.0, 5);
        assert!(approx(&t.values, &[-4.0, -2.0, 0.0, 2.0, 4.0, 6.0, 8.0]));
    }

    #[test]
    fn nice_ticks_degenerate() {
        let t = nice_ticks(0.0, 0.0, 5);
        assert_eq!((t.min, t.max), (0.0, 1.0));
        let t = nice_ticks(5.0, 5.0, 5);
        assert_eq!(t.min, 0.0);
        assert!(t.max >= 5.0);
        let t = nice_ticks(f64::NAN, 3.0, 5);
        assert_eq!((t.min, t.max), (0.0, 1.0));
        let t = nice_ticks(10.0, 0.0, 5);
        assert_eq!((t.min, t.max), (0.0, 10.0));
    }

    #[test]
    fn linear_scale_maps_and_inverts() {
        let s = LinearScale::new((0.0, 100.0), (200.0, 0.0));
        assert_eq!(s.map(0.0), 200.0);
        assert_eq!(s.map(100.0), 0.0);
        assert_eq!(s.map(25.0), 150.0);
        assert!((s.invert(150.0) - 25.0).abs() < 1e-6);
        let flat = LinearScale::new((3.0, 3.0), (10.0, 20.0));
        assert_eq!(flat.map(99.0), 10.0);
    }

    #[test]
    fn band_scale_geometry() {
        let b = BandScale::new(4, 0.0, 100.0, 0.2);
        assert_eq!(b.step(), 25.0);
        assert_eq!(b.band(0), (2.5, 20.0));
        assert_eq!(b.band(3), (77.5, 20.0));
        assert_eq!(b.center(1), 37.5);
    }

    #[test]
    fn band_hit_testing_at_edges() {
        let b = BandScale::new(4, 10.0, 110.0, 0.2);
        assert_eq!(b.index_at(9.999), None);
        assert_eq!(b.index_at(10.0), Some(0));
        assert_eq!(b.index_at(34.999), Some(0));
        assert_eq!(b.index_at(35.0), Some(1));
        assert_eq!(b.index_at(109.999), Some(3));
        assert_eq!(b.index_at(110.0), None);
        assert_eq!(BandScale::new(0, 0.0, 10.0, 0.0).index_at(5.0), None);
    }

    #[test]
    fn layout_maps_values_into_plot() {
        let l = CartesianLayout::new(300.0, 200.0, 10, 87.0, Margins::AXES, 0.2);
        assert_eq!(l.plot.x, 56.0);
        assert_eq!(l.plot.bottom(), 176.0);
        assert_eq!(l.y.map(0.0), l.plot.bottom());
        assert_eq!(l.y.map(l.ticks.max), l.plot.y);
        assert_eq!(l.index_at(l.plot.x, l.plot.y), Some(0));
        assert_eq!(l.index_at(l.plot.x - 1.0, 50.0), None);
        assert_eq!(l.index_at(l.plot.right() - 0.01, 50.0), Some(9));
        assert_eq!(l.index_at(100.0, l.plot.bottom()), None);
    }

    #[test]
    fn labels_thin_out() {
        assert_eq!(label_stride(24, 600.0, 40.0), 2);
        assert_eq!(label_stride(7, 600.0, 40.0), 1);
        assert_eq!(label_stride(30, 300.0, 50.0), 5);
        assert_eq!(label_stride(0, 300.0, 50.0), 1);
    }

    #[test]
    fn tooltip_stays_inside() {
        assert_eq!(
            tooltip_origin((10.0, 50.0), (100.0, 40.0), (400.0, 200.0)),
            (22.0, 30.0)
        );
        assert_eq!(
            tooltip_origin((390.0, 5.0), (100.0, 40.0), (400.0, 200.0)),
            (278.0, 0.0)
        );
        assert_eq!(
            tooltip_origin((50.0, 199.0), (100.0, 40.0), (120.0, 200.0)),
            (20.0, 160.0)
        );
    }
}
