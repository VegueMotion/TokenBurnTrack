use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, Corners, DispatchPhase, Hitbox, HitboxBehavior, Hsla, MouseMoveEvent, Pixels,
    Point, ShapedLine, SharedString, TextAlign, TextRun, Window, fill, point, px, quad, size,
};
use gpui_component::ActiveTheme;

use super::format::ValueKind;
use super::scale::{CartesianLayout, Rect, label_stride, tooltip_origin};

#[derive(Debug, Clone, Copy)]
pub struct ChartTheme {
    pub foreground: Hsla,
    pub muted: Hsla,
    pub grid: Hsla,
    pub axis: Hsla,
    pub surface: Hsla,
    pub tooltip_bg: Hsla,
    pub tooltip_border: Hsla,
    pub highlight: Hsla,
}

impl ChartTheme {
    pub fn from_cx(cx: &App) -> Self {
        let t = cx.theme();
        ChartTheme {
            foreground: t.foreground,
            muted: t.muted_foreground,
            grid: t.border.opacity(0.45),
            axis: t.border,
            surface: t.background,
            tooltip_bg: t.popover,
            tooltip_border: t.border,
            highlight: t.foreground.opacity(0.06),
        }
    }
}

#[derive(Clone, Default)]
pub struct Hover(Rc<Cell<Option<(usize, usize)>>>);

impl Hover {
    pub fn get(&self) -> Option<(usize, usize)> {
        self.0.get()
    }

    pub fn track(
        &self,
        hitbox: &Hitbox,
        origin: Point<Pixels>,
        window: &mut Window,
        hit: impl Fn(f32, f32) -> Option<(usize, usize)> + 'static,
    ) {
        let cell = self.0.clone();
        let hitbox = hitbox.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, _cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let new = if hitbox.is_hovered(window) {
                hit(
                    f32::from(ev.position.x - origin.x),
                    f32::from(ev.position.y - origin.y),
                )
            } else {
                None
            };
            if cell.get() != new {
                cell.set(new);
                window.refresh();
            }
        });
    }
}

pub fn insert_hitbox(bounds: Bounds<Pixels>, window: &mut Window) -> Hitbox {
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub const LABEL_SIZE: f32 = 11.0;

pub fn shape(
    window: &mut Window,
    text: impl Into<SharedString>,
    size: f32,
    color: Hsla,
) -> ShapedLine {
    let text: SharedString = text.into();
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, px(size), &[run], None)
}

pub fn paint_shaped(
    line: &ShapedLine,
    origin: Point<Pixels>,
    (x, y_center): (f32, f32),
    align: Align,
    size: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let w = f32::from(line.width());
    let x0 = match align {
        Align::Left => x,
        Align::Center => x - w / 2.0,
        Align::Right => x - w,
    };
    let lh = size * 1.3;
    line.paint(
        point(origin.x + px(x0), origin.y + px(y_center - lh / 2.0)),
        px(lh),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

#[allow(clippy::too_many_arguments)]
pub fn paint_text(
    text: impl Into<SharedString>,
    origin: Point<Pixels>,
    at: (f32, f32),
    align: Align,
    color: Hsla,
    size: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let line = shape(window, text, size, color);
    paint_shaped(&line, origin, at, align, size, window, cx);
}

pub fn rect_bounds(origin: Point<Pixels>, r: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(r.x), origin.y + px(r.y)),
        size(px(r.w.max(0.0)), px(r.h.max(0.0))),
    )
}

pub fn paint_rect(origin: Point<Pixels>, r: Rect, color: Hsla, window: &mut Window) {
    window.paint_quad(fill(rect_bounds(origin, r), color));
}

pub fn paint_bar(origin: Point<Pixels>, r: Rect, radius: f32, color: Hsla, window: &mut Window) {
    let radius = px(radius.min(r.w / 2.0).min(r.h).max(0.0));
    let corners = Corners {
        top_left: radius,
        top_right: radius,
        bottom_left: px(0.),
        bottom_right: px(0.),
    };
    window.paint_quad(fill(rect_bounds(origin, r), color).corner_radii(corners));
}

pub fn bounds_size(bounds: Bounds<Pixels>) -> (f32, f32) {
    (f32::from(bounds.size.width), f32::from(bounds.size.height))
}

pub fn paint_value_axis(
    origin: Point<Pixels>,
    layout: &CartesianLayout,
    kind: ValueKind,
    theme: &ChartTheme,
    window: &mut Window,
    cx: &mut App,
) {
    let plot = layout.plot;
    for v in &layout.ticks.values {
        let y = layout.y.map(*v);
        let color = if *v == 0.0 { theme.axis } else { theme.grid };
        paint_rect(
            origin,
            Rect {
                x: plot.x,
                y: y.round(),
                w: plot.w,
                h: 1.0,
            },
            color,
            window,
        );
        paint_text(
            kind.fmt_tick(*v, layout.ticks.step),
            origin,
            (plot.x - 8.0, y),
            Align::Right,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }
}

pub fn paint_category_axis(
    origin: Point<Pixels>,
    layout: &CartesianLayout,
    labels: &[SharedString],
    theme: &ChartTheme,
    window: &mut Window,
    cx: &mut App,
) {
    let longest = labels.iter().map(|l| l.len()).max().unwrap_or(1) as f32;
    let stride = label_stride(labels.len(), layout.plot.w, longest * 6.5 + 14.0);
    let y = layout.plot.bottom() + 12.0;
    for (i, label) in labels.iter().enumerate().step_by(stride) {
        paint_text(
            label.clone(),
            origin,
            (layout.x.center(i), y),
            Align::Center,
            theme.muted,
            LABEL_SIZE,
            window,
            cx,
        );
    }
}

pub struct TooltipRow {
    pub swatch: Option<Hsla>,
    pub label: SharedString,
    pub value: SharedString,
}

#[allow(clippy::too_many_arguments)]
pub fn paint_tooltip(
    origin: Point<Pixels>,
    chart_size: (f32, f32),
    anchor: (f32, f32),
    title: &str,
    rows: &[TooltipRow],
    theme: &ChartTheme,
    window: &mut Window,
    cx: &mut App,
) {
    const SIZE: f32 = 12.0;
    const PAD: f32 = 8.0;
    const ROW_H: f32 = 18.0;
    const SWATCH: f32 = 8.0;
    let title_line = shape(window, title.to_owned(), SIZE, theme.foreground);
    let shaped: Vec<(ShapedLine, ShapedLine)> = rows
        .iter()
        .map(|r| {
            (
                shape(window, r.label.clone(), SIZE, theme.muted),
                shape(window, r.value.clone(), SIZE, theme.foreground),
            )
        })
        .collect();
    let has_swatch = rows.iter().any(|r| r.swatch.is_some());
    let swatch_w = if has_swatch { SWATCH + 6.0 } else { 0.0 };
    let label_w = shaped
        .iter()
        .map(|(l, _)| f32::from(l.width()))
        .fold(0.0, f32::max);
    let value_w = shaped
        .iter()
        .map(|(_, v)| f32::from(v.width()))
        .fold(0.0, f32::max);
    let w = (swatch_w + label_w + 16.0 + value_w).max(f32::from(title_line.width())) + PAD * 2.0;
    let h = PAD * 2.0 + ROW_H * (rows.len() + 1) as f32;
    let (x, y) = tooltip_origin(anchor, (w, h), chart_size);
    let b = rect_bounds(origin, Rect { x, y, w, h });
    window.paint_quad(quad(
        b,
        px(6.),
        theme.tooltip_bg,
        px(1.),
        theme.tooltip_border,
        Default::default(),
    ));
    let mut cy = y + PAD + ROW_H / 2.0;
    paint_shaped(
        &title_line,
        origin,
        (x + PAD, cy),
        Align::Left,
        SIZE,
        window,
        cx,
    );
    for (row, (label, value)) in rows.iter().zip(&shaped) {
        cy += ROW_H;
        let mut lx = x + PAD;
        if let Some(c) = row.swatch {
            window.paint_quad(
                fill(
                    rect_bounds(
                        origin,
                        Rect {
                            x: lx,
                            y: cy - SWATCH / 2.0,
                            w: SWATCH,
                            h: SWATCH,
                        },
                    ),
                    c,
                )
                .corner_radii(px(2.)),
            );
        }
        lx += swatch_w;
        paint_shaped(label, origin, (lx, cy), Align::Left, SIZE, window, cx);
        paint_shaped(
            value,
            origin,
            (x + w - PAD, cy),
            Align::Right,
            SIZE,
            window,
            cx,
        );
    }
}
