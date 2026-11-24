use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, Window, div, px,
};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use tbt_core::{HarnessId, Micros};
use tbt_store::{BucketGroup, KpiSummary, ModelStats, ProjectStats, QueryFilter};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_micros, fmt_pct, fmt_tokens, fmt_usd, micros_to_usd};
use crate::charts::time::{bucket_label, bucket_starts, bucket_title, pivot, time_zone};
use crate::charts::{
    BarChart, ChartTheme, HeatMetric, HeatmapChart, Hover, LineChart, LineMode, Series, ValueKind,
    bar_chart, heatmap_chart, legend, line_chart, sparkline,
};
use crate::data::{self, DataSource, NO_DATA_MESSAGE};
use crate::palette::{TokenKind, categorical, harness_color, harness_label};
use crate::ui;

const TOP_N: usize = 6;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub struct DashboardData {
    pub kpi: KpiSummary,
    pub cost: BarChart,
    pub tokens: LineChart,
    pub heat: HeatmapChart,
    pub cost_per_bucket: Vec<f64>,
    pub top_models: Vec<ModelStats>,
    pub top_projects: Vec<ProjectStats>,
    pub no_data: bool,
}

impl DashboardData {
    pub fn load(data: &dyn DataSource, filter: &QueryFilter) -> anyhow::Result<DashboardData> {
        let kpi = data.kpi_summary(filter)?;
        let no_data = kpi.events == 0 && !data.has_data()?;
        let tz = time_zone(&filter.time_zone);
        let starts = bucket_starts(filter.range, filter.bucket, &tz);
        let labels: Vec<SharedString> = starts
            .iter()
            .map(|s| bucket_label(*s, filter.bucket, &tz).into())
            .collect();
        let titles: Vec<SharedString> = starts
            .iter()
            .map(|s| bucket_title(*s, filter.bucket, &tz).into())
            .collect();

        let buckets = data.cost_buckets(filter, BucketGroup::Harness)?;
        let by_harness = pivot(&buckets, &starts, |b| micros_to_usd(b.cost));
        let cost_series: Vec<Series> = by_harness
            .into_iter()
            .map(|(key, values)| {
                let id = HarnessId::new(key);
                Series {
                    name: harness_label(&id).into(),
                    color: harness_color(&id),
                    values,
                }
            })
            .collect();
        let cost_per_bucket = crate::charts::stack::totals(
            &cost_series
                .iter()
                .map(|s| s.values.clone())
                .collect::<Vec<_>>(),
        );

        let token_series: Vec<Series> = TokenKind::ALL
            .iter()
            .map(|kind| {
                let per_key = pivot(&buckets, &starts, |b| kind.of(&b.tokens) as f64);
                let mut values = vec![0.0; starts.len()];
                for v in per_key.values() {
                    for (acc, x) in values.iter_mut().zip(v) {
                        *acc += x;
                    }
                }
                Series {
                    name: kind.label().into(),
                    color: kind.color(),
                    values,
                }
            })
            .collect();

        let mut cost_grid = vec![vec![0.0; 24]; 7];
        let mut event_grid = vec![vec![0.0; 24]; 7];
        for c in data.heatmap(filter)? {
            let (r, h) = (c.weekday as usize, c.hour as usize);
            if r < 7 && h < 24 {
                cost_grid[r][h] += micros_to_usd(c.cost);
                event_grid[r][h] += c.events as f64;
            }
        }

        let mut top_models = data.model_stats(filter)?;
        top_models.truncate(TOP_N);
        let mut top_projects = data.project_stats(filter)?;
        top_projects.truncate(TOP_N);

        Ok(DashboardData {
            kpi,
            cost: BarChart {
                labels: labels.clone(),
                titles: titles.clone(),
                series: cost_series,
                kind: ValueKind::Usd,
            },
            tokens: LineChart {
                labels,
                titles,
                series: token_series,
                kind: ValueKind::Tokens,
                mode: LineMode::StackedArea,
            },
            heat: HeatmapChart {
                row_labels: WEEKDAYS.iter().map(|d| SharedString::from(*d)).collect(),
                col_labels: (0..24).map(|h| format!("{h:02}:00").into()).collect(),
                metrics: vec![
                    HeatMetric {
                        name: "Cost".into(),
                        kind: ValueKind::Usd,
                        values: cost_grid,
                    },
                    HeatMetric {
                        name: "Calls".into(),
                        kind: ValueKind::Count,
                        values: event_grid,
                    },
                ],
            },
            cost_per_bucket,
            top_models,
            top_projects,
            no_data,
        })
    }
}

pub fn burn_rate(kpi: &KpiSummary) -> Option<f64> {
    (kpi.active_hours > 0).then(|| micros_to_usd(kpi.cost) / kpi.active_hours as f64)
}

pub fn cost_with_unpriced(cost: Micros, unpriced: u64) -> String {
    match unpriced {
        0 => fmt_micros(cost),
        n if cost == Micros::ZERO => format!("{n} unpriced"),
        n => format!("{} + {n} unpriced", fmt_micros(cost)),
    }
}

pub fn unpriced_badge(unpriced: u64, cx: &App) -> Option<AnyElement> {
    let t = cx.theme();
    (unpriced > 0).then(|| {
        div()
            .flex_none()
            .px_1p5()
            .rounded(px(4.))
            .border_1()
            .border_color(t.warning.opacity(0.5))
            .bg(t.warning.opacity(0.12))
            .text_xs()
            .text_color(t.warning)
            .child(format!("⚠ + {} unpriced", fmt_count(unpriced)))
            .into_any_element()
    })
}

#[derive(Default)]
struct Hovers {
    cost: Hover,
    tokens: Hover,
    heat: Hover,
}

pub struct DashboardPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<DashboardData>,
    hovers: Hovers,
    _load: Task<()>,
    _subscription: Subscription,
}

impl DashboardPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&app_state, |this, _, _: &AppStateEvent, cx| {
            this.reload(cx);
        });
        let mut page = DashboardPage {
            app_state,
            data,
            state: Loadable::Loading,
            hovers: Hovers::default(),
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(data::now());
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || DashboardData::load(&*data, &filter),
            |this, state, _| this.state = state,
        );
    }
}

fn kpi_tile(
    label: &'static str,
    value: String,
    badge: Option<AnyElement>,
    sub: Option<String>,
    extra: Option<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    let t = cx.theme();
    v_flex()
        .flex_1()
        .min_w(px(150.))
        .gap_1()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(t.border)
        .bg(t.secondary.opacity(0.35))
        .child(div().text_xs().text_color(t.muted_foreground).child(label))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_2xl()
                        .font_semibold()
                        .text_color(t.foreground)
                        .child(value),
                )
                .children(badge),
        )
        .child(
            div()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(sub.unwrap_or_default()),
        )
        .children(extra)
}

fn ranked_row(
    name: String,
    detail: String,
    value: String,
    badge: Option<AnyElement>,
    fraction: f64,
    color: Hsla,
    cx: &App,
) -> impl IntoElement {
    let t = cx.theme();
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(t.foreground)
                        .child(name),
                )
                .child(div().text_xs().text_color(t.muted_foreground).child(detail))
                .children(badge)
                .child(
                    div()
                        .w(px(72.))
                        .flex()
                        .justify_end()
                        .text_color(t.foreground)
                        .child(value),
                ),
        )
        .child(ui::bar_cell(fraction, color, t.border.opacity(0.4)))
}

impl DashboardPage {
    fn render_ready(&self, d: &DashboardData, cx: &App) -> AnyElement {
        let theme = ChartTheme::from_cx(cx);
        let t = cx.theme();
        let fg = t.foreground;
        let k = &d.kpi;
        let tiles = h_flex()
            .items_stretch()
            .gap_3()
            .flex_wrap()
            .child(kpi_tile(
                "Cost in range",
                fmt_micros(k.cost),
                unpriced_badge(k.unpriced_events, cx),
                Some(if k.unpriced_events > 0 {
                    "unpriced calls not counted".into()
                } else {
                    "API-equivalent cost".into()
                }),
                Some(
                    sparkline(d.cost_per_bucket.clone(), categorical(0))
                        .w_full()
                        .h(px(28.))
                        .into_any_element(),
                ),
                cx,
            ))
            .child(kpi_tile(
                "Total tokens",
                fmt_tokens(k.tokens.total()),
                None,
                Some(format!(
                    "{} in · {} out",
                    fmt_tokens(k.tokens.input + k.tokens.cache_write + k.tokens.cache_read),
                    fmt_tokens(k.tokens.output)
                )),
                None,
                cx,
            ))
            .child(kpi_tile(
                "Sessions",
                fmt_count(k.sessions),
                None,
                Some(format!("{} calls", fmt_count(k.events))),
                None,
                cx,
            ))
            .child(kpi_tile(
                "Cache hit",
                k.tokens
                    .cache_hit_ratio()
                    .map(fmt_pct)
                    .unwrap_or_else(|| "–".into()),
                None,
                Some("of prompt tokens".into()),
                None,
                cx,
            ))
            .child(kpi_tile(
                "Burn rate",
                burn_rate(k)
                    .map(|r| format!("{}/h", fmt_usd(r)))
                    .unwrap_or_else(|| "–".into()),
                None,
                Some(format!("over {} active hours", k.active_hours)),
                None,
                cx,
            ));

        let cost_legend = legend(
            d.cost.series.iter().map(|s| (s.name.clone(), s.color)),
            t.muted_foreground,
        );
        let token_legend = legend(
            d.tokens.series.iter().map(|s| (s.name.clone(), s.color)),
            t.muted_foreground,
        );

        let max_model = d
            .top_models
            .first()
            .map(|m| micros_to_usd(m.cost))
            .unwrap_or(0.0);
        let models = ui::card("Top models", cx)
            .flex_1()
            .children(if d.top_models.is_empty() {
                vec![ui::muted_text("No usage in range", cx).into_any_element()]
            } else {
                d.top_models
                    .iter()
                    .map(|m| {
                        ranked_row(
                            m.model.clone(),
                            format!("{} tok", fmt_tokens(m.tokens.total())),
                            if m.unpriced_events > 0 && m.cost == Micros::ZERO {
                                "–".into()
                            } else {
                                fmt_micros(m.cost)
                            },
                            unpriced_badge(m.unpriced_events, cx),
                            safe_ratio(micros_to_usd(m.cost), max_model),
                            categorical(0),
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect()
            });
        let max_project = d
            .top_projects
            .first()
            .map(|p| micros_to_usd(p.cost))
            .unwrap_or(0.0);
        let projects =
            ui::card("Top projects", cx)
                .flex_1()
                .children(if d.top_projects.is_empty() {
                    vec![ui::muted_text("No usage in range", cx).into_any_element()]
                } else {
                    d.top_projects
                        .iter()
                        .map(|p| {
                            ranked_row(
                                p.name.clone().unwrap_or_else(|| "(no project)".into()),
                                format!("{} sessions", p.sessions),
                                fmt_micros(p.cost),
                                None,
                                safe_ratio(micros_to_usd(p.cost), max_project),
                                categorical(0),
                                cx,
                            )
                            .into_any_element()
                        })
                        .collect()
                });

        v_flex()
            .gap_4()
            .p_4()
            .text_color(fg)
            .child(tiles)
            .child(
                ui::card("Cost by harness", cx).child(cost_legend).child(
                    bar_chart(d.cost.clone(), self.hovers.cost.clone(), theme)
                        .w_full()
                        .h(px(240.)),
                ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        ui::card("Tokens by type", cx)
                            .flex_1()
                            .min_w(px(300.))
                            .child(token_legend)
                            .child(
                                line_chart(d.tokens.clone(), self.hovers.tokens.clone(), theme)
                                    .w_full()
                                    .h(px(220.)),
                            ),
                    )
                    .child(
                        ui::card("Activity by weekday and hour", cx)
                            .flex_1()
                            .min_w(px(300.))
                            .child(ui::muted_text("Cost per hour of day, local time", cx))
                            .child(
                                heatmap_chart(d.heat.clone(), self.hovers.heat.clone(), theme)
                                    .w_full()
                                    .h(px(220.)),
                            ),
                    ),
            )
            .child(h_flex().gap_4().items_start().child(models).child(projects))
            .into_any_element()
    }
}

fn safe_ratio(v: f64, max: f64) -> f64 {
    if max > 0.0 { v / max } else { 0.0 }
}

impl Render for DashboardPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(d) if d.no_data => ui::status_message(NO_DATA_MESSAGE, cx),
            Loadable::Ready(d) => self.render_ready(d, cx),
        };
        ui::page_scroll("dashboard-scroll", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::MockData;
    use jiff::Timestamp;
    use tbt_core::TimeRange;
    use tbt_store::BucketSize;

    #[test]
    fn burn_rate_per_active_hour() {
        let k = KpiSummary {
            cost: Micros(12_000_000),
            active_hours: 4,
            ..Default::default()
        };
        assert_eq!(burn_rate(&k), Some(3.0));
        assert_eq!(burn_rate(&KpiSummary::default()), None);
    }

    #[test]
    fn load_builds_dense_series() {
        let now: Timestamp = "2026-11-23T15:20:00Z".parse().unwrap();
        let data = MockData::new(11, now);
        let filter = QueryFilter::new(
            TimeRange::new(
                "2026-11-17T00:00:00Z".parse().unwrap(),
                "2026-11-24T00:00:00Z".parse().unwrap(),
            ),
            BucketSize::Day,
        );
        let d = DashboardData::load(&data, &filter).unwrap();
        assert_eq!(d.cost.labels.len(), 7);
        assert!(d.cost.series.iter().all(|s| s.values.len() == 7));
        let total: f64 = d.cost_per_bucket.iter().sum();
        assert!((total - micros_to_usd(d.kpi.cost)).abs() < 1e-6);
        let tokens: f64 = d.tokens.series.iter().flat_map(|s| s.values.iter()).sum();
        assert_eq!(tokens as u64, d.kpi.tokens.total());
        assert!(d.top_models.len() <= TOP_N);
        assert!(!d.no_data);
    }

    #[test]
    fn unpriced_cost_is_not_shown_as_zero() {
        assert_eq!(cost_with_unpriced(Micros(12_340_000), 0), "$12.34");
        assert_eq!(
            cost_with_unpriced(Micros(12_340_000), 5),
            "$12.34 + 5 unpriced"
        );
        assert_eq!(cost_with_unpriced(Micros::ZERO, 3), "3 unpriced");
        assert_eq!(cost_with_unpriced(Micros::ZERO, 0), "$0.00");
    }
}
