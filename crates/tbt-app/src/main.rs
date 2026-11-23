use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, Entity, Hsla, PathBuilder, Pixels, ScrollStrategy, SharedString, Task,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, canvas, div, fill, point,
    prelude::*, px, size, uniform_list,
};
use gpui_component::{
    ActiveTheme, Root, Theme, ThemeMode,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    v_flex,
};

const ROW_COUNT: usize = 100_000;

struct Row {
    id: usize,
    tokens: u64,
    cost_micros: u64,
}

struct Spike {
    rows: Vec<Row>,
    series: Vec<f32>,
    background_result: Option<(u64, Duration)>,
    scroll: UniformListScrollHandle,
    _job: Task<()>,
}

impl Spike {
    fn new(cx: &mut Context<Self>) -> Self {
        let rows = (0..ROW_COUNT)
            .map(|id| Row {
                id,
                tokens: (id as u64 * 7_919) % 250_000,
                cost_micros: (id as u64 * 104_729) % 9_000_000,
            })
            .collect();
        let series = (0..48)
            .map(|i| 0.5 + 0.4 * ((i as f32) * 0.35).sin())
            .collect();
        let job = cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let sum = cx
                .background_executor()
                .spawn(async move {
                    (0..50_000_000u64).fold(0u64, |acc, n| acc.wrapping_add(n * n % 7))
                })
                .await;
            let elapsed = started.elapsed();
            this.update(cx, |this, cx| {
                this.background_result = Some((sum, elapsed));
                this.scroll
                    .scroll_to_item(ROW_COUNT - 1, ScrollStrategy::Bottom);
                cx.notify();
            })
            .ok();
        });
        Self {
            rows,
            series,
            background_result: None,
            scroll: UniformListScrollHandle::new(),
            _job: job,
        }
    }

    fn chart(&self, bar: Hsla, line: Hsla, grid: Hsla) -> impl IntoElement {
        let series = self.series.clone();
        canvas(
            |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, (), window, _cx| {
                let origin = bounds.origin;
                let w = bounds.size.width;
                let h = bounds.size.height;
                window.paint_quad(fill(
                    Bounds::new(point(origin.x, origin.y + h - px(1.)), size(w, px(1.))),
                    grid,
                ));
                let slot = w / series.len() as f32;
                for (i, v) in series.iter().enumerate() {
                    let bar_h = h * *v;
                    window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x + slot * i as f32 + px(1.), origin.y + h - bar_h),
                            size(slot - px(2.), bar_h),
                        ),
                        bar,
                    ));
                }
                let mut path = PathBuilder::stroke(px(2.));
                for (i, v) in series.iter().enumerate() {
                    let p = point(
                        origin.x + slot * i as f32 + slot / 2.,
                        origin.y + h - h * (1. - *v) * 0.9,
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, line);
                }
            },
        )
        .w_full()
        .h(px(160.))
    }
}

impl Render for Spike {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bar, line, grid) = (theme.primary, theme.chart_bullish, theme.border);
        let (bg, fg, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        let status: SharedString = match self.background_result {
            Some((sum, elapsed)) => format!("background sum = {sum} in {elapsed:.2?}").into(),
            None => "background task running...".into(),
        };

        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Label::new("TokenBurnTrack GPUI spike"))
                    .child(div().text_color(muted).child(status))
                    .child(
                        Button::new("top")
                            .primary()
                            .label("Top")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                                cx.notify();
                            })),
                    ),
            )
            .child(self.chart(bar, line, grid))
            .child(
                uniform_list(
                    "rows",
                    self.rows.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                        range
                            .map(|ix| {
                                let row = &this.rows[ix];
                                h_flex()
                                    .id(ix)
                                    .w_full()
                                    .h(px(22.))
                                    .gap_4()
                                    .px_2()
                                    .when(ix % 2 == 0, |d| d.bg(muted.opacity(0.08)))
                                    .child(div().w(px(80.)).child(format!("#{}", row.id)))
                                    .child(div().w(px(120.)).child(format!("{} tok", row.tokens)))
                                    .child(format!("${:.6}", row.cost_micros as f64 / 1_000_000.0))
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1(),
            )
    }
}

fn open(cx: &mut App) -> anyhow::Result<()> {
    let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("tbt-app".into()),
            ..Default::default()
        },
        |window, cx| {
            let view: Entity<Spike> = cx.new(Spike::new);
            cx.new(|cx| Root::new(view, window, cx))
        },
    )?;
    cx.activate(true);
    Ok(())
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        if let Err(err) = open(cx) {
            eprintln!("failed to open window: {err:#}");
            cx.quit();
        }
    });
}
