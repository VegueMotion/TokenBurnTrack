use std::cell::Cell;
use std::cmp::Reverse;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, Sizable, StyledExt, h_flex, v_flex};
use jiff::SignedDuration;
use tbt_core::HarnessId;
use tbt_store::{BucketGroup, BucketSize, KpiSummary, ModelStats, ProjectStats, QueryFilter};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_micros, fmt_pct, fmt_tokens, fmt_usd, micros_to_usd};
use crate::charts::time::{
    bucket_index, bucket_label, bucket_starts, bucket_title, pivot, time_zone,
};
use crate::charts::{
    BarChart, ChartTheme, HeatMetric, HeatmapChart, Hover, LineChart, LineMode, Series, ValueKind,
    bar_chart, legend, line_chart, sparkline,
};
use crate::data::{self, DataSource, NO_DATA_MESSAGE};
use crate::palette::{TokenKind, categorical, harness_color, harness_label};
use crate::ui;

const TOP_N: usize = 6;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Metric {
    #[default]
    Cost,
    Tokens,
}

impl Metric {
    pub const ALL: [Metric; 2] = [Metric::Cost, Metric::Tokens];

    pub fn label(self) -> &'static str {
        match self {
            Metric::Cost => "Cost",
            Metric::Tokens => "Tokens",
        }
    }

    fn heat_metric(self) -> HeatMetric {
        match self {
            Metric::Cost => HeatMetric::Cost,
            Metric::Tokens => HeatMetric::Tokens,
        }
    }
}

fn top_n<T: Clone>(by_cost: &[T], tokens: impl Fn(&T) -> u64) -> (Vec<T>, Vec<T>) {
    let mut by_tokens = by_cost.to_vec();
    by_tokens.sort_by_key(|x| Reverse(tokens(x)));
    by_tokens.truncate(TOP_N);
    (by_cost.iter().take(TOP_N).cloned().collect(), by_tokens)
}

pub struct DashboardData {
    pub kpi: KpiSummary,
    pub cost: BarChart,
    pub harness_tokens: BarChart,
    pub tokens: LineChart,
    pub type_cost: LineChart,
    pub heat: HeatmapChart,
    pub cost_per_bucket: Vec<f64>,
    pub top_models: Vec<ModelStats>,
    pub top_projects: Vec<ProjectStats>,
    pub top_models_by_tokens: Vec<ModelStats>,
    pub top_projects_by_tokens: Vec<ProjectStats>,
    pub no_data: bool,
}

impl DashboardData {
    pub fn type_chart(&self, metric: Metric) -> &LineChart {
        match metric {
            Metric::Cost => &self.type_cost,
            Metric::Tokens => &self.tokens,
        }
    }

    pub fn harness_chart(&self, metric: Metric) -> &BarChart {
        match metric {
            Metric::Cost => &self.cost,
            Metric::Tokens => &self.harness_tokens,
        }
    }

    pub fn models(&self, metric: Metric) -> &[ModelStats] {
        match metric {
            Metric::Cost => &self.top_models,
            Metric::Tokens => &self.top_models_by_tokens,
        }
    }

    pub fn projects(&self, metric: Metric) -> &[ProjectStats] {
        match metric {
            Metric::Cost => &self.top_projects,
            Metric::Tokens => &self.top_projects_by_tokens,
        }
    }
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
        let harness_series = |value: fn(&tbt_store::CostBucket) -> f64| -> Vec<Series> {
            pivot(&buckets, &starts, value)
                .into_iter()
                .map(|(key, values)| {
                    let id = HarnessId::new(key);
                    Series {
                        name: harness_label(&id).into(),
                        color: harness_color(&id),
                        values,
                    }
                })
                .collect()
        };
        let cost_series = harness_series(|b| micros_to_usd(b.cost));
        let harness_token_series = harness_series(|b| b.tokens.total() as f64);
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

        let type_costs = data.type_cost_buckets(filter)?;
        let type_cost_series: Vec<Series> = TokenKind::ALL
            .iter()
            .map(|kind| {
                let mut values = vec![0.0; starts.len()];
                for b in &type_costs {
                    if let Some(i) = bucket_index(&starts, b.start) {
                        values[i] += micros_to_usd(kind.cost_of(b));
                    }
                }
                Series {
                    name: kind.label().into(),
                    color: kind.color(),
                    values,
                }
            })
            .collect();

        let last_day = (filter.range.end - SignedDuration::from_secs(1))
            .to_zoned_datetime(tz.clone())
            .date();
        let year = QueryFilter {
            range: HeatmapChart::range_ending(last_day, &tz).unwrap_or(filter.range),
            bucket: BucketSize::Day,
            ..filter.clone()
        };
        let heat = HeatmapChart::from_buckets(
            &data.cost_buckets(&year, BucketGroup::Harness)?,
            last_day,
            &tz,
        );

        let (top_models, top_models_by_tokens) =
            top_n(&data.model_stats(filter)?, |m| m.tokens.total());
        let (top_projects, top_projects_by_tokens) =
            top_n(&data.project_stats(filter)?, |p| p.tokens.total());

        Ok(DashboardData {
            kpi,
            cost: BarChart {
                labels: labels.clone(),
                titles: titles.clone(),
                series: cost_series,
                kind: ValueKind::Usd,
            },
            harness_tokens: BarChart {
                labels: labels.clone(),
                titles: titles.clone(),
                series: harness_token_series,
                kind: ValueKind::Tokens,
            },
            tokens: LineChart {
                labels: labels.clone(),
                titles: titles.clone(),
                series: token_series,
                kind: ValueKind::Tokens,
                mode: LineMode::StackedArea,
            },
            type_cost: LineChart {
                labels,
                titles,
                series: type_cost_series,
                kind: ValueKind::Usd,
                mode: LineMode::StackedArea,
            },
            heat,
            cost_per_bucket,
            top_models,
            top_projects,
            top_models_by_tokens,
            top_projects_by_tokens,
            no_data,
        })
    }
}

pub fn burn_rate(kpi: &KpiSummary) -> Option<f64> {
    (kpi.active_hours > 0).then(|| micros_to_usd(kpi.cost) / kpi.active_hours as f64)
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
    heat_metric: Rc<Cell<HeatMetric>>,
    metric: Rc<Cell<Metric>>,
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
            heat_metric: Rc::default(),
            metric: Rc::default(),
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
            div()
                .text_2xl()
                .font_semibold()
                .text_color(t.foreground)
                .child(value),
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
                Some("API-equivalent cost".into()),
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
                Some("of prompt tokens".into()),
                None,
                cx,
            ))
            .child(kpi_tile(
                "Burn rate",
                burn_rate(k)
                    .map(|r| format!("{}/h", fmt_usd(r)))
                    .unwrap_or_else(|| "–".into()),
                Some(format!("over {} active hours", k.active_hours)),
                None,
                cx,
            ));

        let metric = self.metric.get();
        let harness = d.harness_chart(metric);
        let cost_legend = legend(
            harness.series.iter().map(|s| (s.name.clone(), s.color)),
            t.muted_foreground,
        );
        let by_type = d.type_chart(metric);
        let token_legend = legend(
            by_type.series.iter().map(|s| (s.name.clone(), s.color)),
            t.muted_foreground,
        );

        let models = d.models(metric);
        let model_value = |m: &ModelStats| match metric {
            Metric::Cost => micros_to_usd(m.cost),
            Metric::Tokens => m.tokens.total() as f64,
        };
        let max_model = models.first().map(model_value).unwrap_or(0.0);
        let models = ui::card("Top models", cx)
            .flex_1()
            .children(if models.is_empty() {
                vec![ui::muted_text("No usage in range", cx).into_any_element()]
            } else {
                models
                    .iter()
                    .map(|m| {
                        let (detail, value) = match metric {
                            Metric::Cost => (
                                format!("{} tok", fmt_tokens(m.tokens.total())),
                                fmt_micros(m.cost),
                            ),
                            Metric::Tokens => (fmt_micros(m.cost), fmt_tokens(m.tokens.total())),
                        };
                        ranked_row(
                            m.model.clone(),
                            detail,
                            value,
                            safe_ratio(model_value(m), max_model),
                            categorical(0),
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect()
            });
        let projects = d.projects(metric);
        let project_value = |p: &ProjectStats| match metric {
            Metric::Cost => micros_to_usd(p.cost),
            Metric::Tokens => p.tokens.total() as f64,
        };
        let max_project = projects.first().map(project_value).unwrap_or(0.0);
        let projects = ui::card("Top projects", cx)
            .flex_1()
            .children(if projects.is_empty() {
                vec![ui::muted_text("No usage in range", cx).into_any_element()]
            } else {
                projects
                    .iter()
                    .map(|p| {
                        let value = match metric {
                            Metric::Cost => fmt_micros(p.cost),
                            Metric::Tokens => fmt_tokens(p.tokens.total()),
                        };
                        ranked_row(
                            p.name.clone().unwrap_or_else(|| "(no project)".into()),
                            format!("{} sessions", p.sessions),
                            value,
                            safe_ratio(project_value(p), max_project),
                            categorical(0),
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect()
            });

        let toggle = h_flex()
            .justify_end()
            .gap_1()
            .children(Metric::ALL.into_iter().map(|m| {
                let cell = self.metric.clone();
                let heat = self.heat_metric.clone();
                Button::new(SharedString::from(format!(
                    "dashboard-metric-{}",
                    m.label()
                )))
                .small()
                .ghost()
                .selected(metric == m)
                .label(m.label())
                .on_click(move |_, window, _| {
                    cell.set(m);
                    heat.set(m.heat_metric());
                    window.refresh();
                })
            }));
        let (harness_title, type_title) = match metric {
            Metric::Cost => ("Cost by harness", "Cost by type"),
            Metric::Tokens => ("Tokens by harness", "Tokens by type"),
        };

        v_flex()
            .gap_4()
            .p_4()
            .text_color(fg)
            .child(tiles)
            .child(toggle)
            .child(
                ui::card(harness_title, cx).child(cost_legend).child(
                    bar_chart(harness.clone(), self.hovers.cost.clone(), theme)
                        .w_full()
                        .h(px(240.)),
                ),
            )
            .child(
                ui::card(type_title, cx).child(token_legend).child(
                    line_chart(by_type.clone(), self.hovers.tokens.clone(), theme)
                        .w_full()
                        .h(px(220.)),
                ),
            )
            .child(ui::card("Activity heatmap", cx).child(d.heat.view(
                self.heat_metric.clone(),
                self.hovers.heat.clone(),
                theme,
                cx,
            )))
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
    use tbt_core::{Micros, TimeRange};
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
    fn top_by_tokens_ranks_before_truncating() {
        let by_cost: Vec<(u64, u64)> = (0..10).map(|i| (10 - i, i * 100)).collect();
        let (cost, tokens) = top_n(&by_cost, |x| x.1);
        assert_eq!(cost, by_cost[..TOP_N]);
        let ranked: Vec<u64> = tokens.iter().map(|x| x.1).collect();
        assert_eq!(ranked, [900, 800, 700, 600, 500, 400]);
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
        assert!(d.top_models_by_tokens.len() <= TOP_N);
        let type_cost: f64 = d
            .type_cost
            .series
            .iter()
            .flat_map(|s| s.values.iter())
            .sum();
        assert!((type_cost - micros_to_usd(d.kpi.cost)).abs() < 0.01);
        let harness_tokens: f64 = d
            .harness_tokens
            .series
            .iter()
            .flat_map(|s| s.values.iter())
            .sum();
        assert_eq!(harness_tokens as u64, d.kpi.tokens.total());
        assert!(
            d.top_models_by_tokens
                .windows(2)
                .all(|w| w[0].tokens.total() >= w[1].tokens.total())
        );
        assert!(!d.no_data);
    }
}
