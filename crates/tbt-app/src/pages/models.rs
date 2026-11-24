use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, Window, div, hsla, px,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use tbt_core::Micros;
use tbt_store::{BucketGroup, ModelStats, QueryFilter};

use super::dashboard::{cost_with_unpriced, unpriced_badge};
use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_micros, fmt_tokens, fmt_usd, micros_to_usd};
use crate::charts::time::{bucket_label, bucket_starts, bucket_title, pivot, time_zone};
use crate::charts::{
    BarChart, ChartTheme, Hover, LineChart, LineMode, Series, ValueKind, bar_chart, legend,
    line_chart,
};
use crate::data::{self, DataSource, NO_DATA_MESSAGE};
use crate::palette::categorical;
use crate::ui::{self, Col};

pub const TOP_MODELS: usize = 7;
pub const OTHER: &str = "Other";

pub fn other_color() -> Hsla {
    hsla(0.0, 0.0, 0.55, 1.0)
}

pub fn per_mtok(cost: Micros, tokens: u64) -> Option<f64> {
    (tokens > 0).then(|| micros_to_usd(cost) / tokens as f64 * 1e6)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub total: Option<f64>,
    pub prompt: Option<f64>,
    pub output: Option<f64>,
}

pub fn rates(m: &ModelStats) -> Rates {
    if m.unpriced_events > 0 {
        return Rates::default();
    }
    let t = &m.tokens;
    Rates {
        total: per_mtok(m.cost, t.total()),
        prompt: per_mtok(m.cost, t.input + t.cache_write + t.cache_read),
        output: per_mtok(m.cost, t.output),
    }
}

pub fn rank_models(mut models: Vec<ModelStats>) -> Vec<ModelStats> {
    models.sort_by(|a, b| {
        b.cost
            .cmp(&a.cost)
            .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
            .then_with(|| a.model.cmp(&b.model))
    });
    models
}

pub fn model_order(models: &[ModelStats]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in models {
        if !out.contains(&m.model) {
            out.push(m.model.clone());
        }
    }
    out
}

pub fn group_top(
    order: &[String],
    mut pivoted: BTreeMap<String, Vec<f64>>,
    top: usize,
) -> Vec<(String, Vec<f64>)> {
    let mut out: Vec<(String, Vec<f64>)> = order
        .iter()
        .take(top)
        .filter_map(|k| pivoted.remove(k).map(|v| (k.clone(), v)))
        .collect();
    let mut other: Option<Vec<f64>> = None;
    for values in pivoted.into_values() {
        let acc = other.get_or_insert_with(|| vec![0.0; values.len()]);
        for (a, v) in acc.iter_mut().zip(values) {
            *a += v;
        }
    }
    if let Some(v) = other {
        out.push((OTHER.to_owned(), v));
    }
    out
}

fn colors(order: &[String]) -> HashMap<String, Hsla> {
    order
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let c = if i < TOP_MODELS {
                categorical(i)
            } else {
                other_color()
            };
            (k.clone(), c)
        })
        .collect()
}

pub struct ModelRow {
    pub stats: ModelStats,
    pub color: Hsla,
    pub rates: Rates,
}

pub struct ModelsData {
    pub cost: BarChart,
    pub tokens: LineChart,
    pub rows: Vec<ModelRow>,
    pub cost_total: Micros,
    pub unpriced: u64,
    pub savings: Micros,
    pub no_data: bool,
}

impl ModelsData {
    pub fn load(data: &dyn DataSource, filter: &QueryFilter) -> anyhow::Result<ModelsData> {
        let models = rank_models(data.model_stats(filter)?);
        let no_data = models.is_empty() && !data.has_data()?;
        let order = model_order(&models);
        let palette = colors(&order);
        let color_of = |k: &str| palette.get(k).copied().unwrap_or_else(other_color);

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
        let buckets = data.cost_buckets(filter, BucketGroup::Model)?;
        let series = |values: BTreeMap<String, Vec<f64>>| -> Vec<Series> {
            group_top(&order, values, TOP_MODELS)
                .into_iter()
                .map(|(k, values)| Series {
                    color: color_of(&k),
                    name: k.into(),
                    values,
                })
                .collect()
        };
        let cost_series = series(pivot(&buckets, &starts, |b| micros_to_usd(b.cost)));
        let token_series = series(pivot(&buckets, &starts, |b| b.tokens.total() as f64));

        Ok(ModelsData {
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
            cost_total: models.iter().map(|m| m.cost).sum(),
            unpriced: models.iter().map(|m| m.unpriced_events).sum(),
            savings: models.iter().map(|m| m.cache_savings).sum(),
            rows: models
                .into_iter()
                .map(|m| ModelRow {
                    color: color_of(&m.model),
                    rates: rates(&m),
                    stats: m,
                })
                .collect(),
            no_data,
        })
    }
}

fn fmt_rate(r: Option<f64>) -> String {
    r.map(fmt_usd).unwrap_or_else(|| "–".into())
}

#[derive(Default)]
struct Hovers {
    cost: Hover,
    tokens: Hover,
}

pub struct ModelsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<Arc<ModelsData>>,
    hovers: Hovers,
    _load: Task<()>,
    _subscription: Subscription,
}

impl ModelsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&app_state, |this, _, ev: &AppStateEvent, cx| {
            if matches!(
                ev,
                AppStateEvent::FiltersChanged | AppStateEvent::DataChanged
            ) {
                this.reload(cx);
            }
        });
        let mut page = ModelsPage {
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
            move || Ok(Arc::new(ModelsData::load(&*data, &filter)?)),
            |this, state, _| this.state = state,
        );
    }

    fn render_table(&self, d: &ModelsData, cx: &App) -> AnyElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let cols = [
            Col::flex("Model"),
            Col::fixed("Provider", 80.0),
            Col::num("Calls", 56.0),
            Col::num("Input", 62.0),
            Col::num("Output", 62.0),
            Col::num("Cache write", 76.0),
            Col::num("Cache read", 72.0),
            Col::num("Cost", 130.0),
            Col::num("$/Mtok", 64.0),
            Col::num("$/M prompt", 74.0),
            Col::num("$/M output", 74.0),
            Col::num("Cache savings", 92.0),
        ];
        let rows = d
            .rows
            .iter()
            .map(|r| {
                let m = &r.stats;
                let cost = if m.unpriced_events > 0 && m.cost == Micros::ZERO {
                    "–".to_owned()
                } else {
                    fmt_micros(m.cost)
                };
                vec![
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(ui::swatch(r.color))
                        .child(m.model.clone())
                        .into_any_element(),
                    div()
                        .text_color(muted)
                        .child(m.provider.clone())
                        .into_any_element(),
                    fmt_count(m.events).into_any_element(),
                    fmt_tokens(m.tokens.input).into_any_element(),
                    fmt_tokens(m.tokens.output).into_any_element(),
                    fmt_tokens(m.tokens.cache_write).into_any_element(),
                    fmt_tokens(m.tokens.cache_read).into_any_element(),
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .children(unpriced_badge(m.unpriced_events, cx))
                        .child(cost)
                        .into_any_element(),
                    fmt_rate(r.rates.total).into_any_element(),
                    fmt_rate(r.rates.prompt).into_any_element(),
                    fmt_rate(r.rates.output).into_any_element(),
                    fmt_micros(m.cache_savings).into_any_element(),
                ]
            })
            .collect();
        ui::table(&cols, rows, cx).into_any_element()
    }

    fn render_ready(&self, d: &ModelsData, cx: &App) -> AnyElement {
        let theme = ChartTheme::from_cx(cx);
        let muted = cx.theme().muted_foreground;
        let legend_of = |series: &[Series]| {
            let items: Vec<(SharedString, Hsla)> =
                series.iter().map(|s| (s.name.clone(), s.color)).collect();
            legend(items, muted)
        };
        let summary = format!(
            "{} models · {} · cache reads saved {}",
            d.rows.len(),
            cost_with_unpriced(d.cost_total, d.unpriced),
            fmt_micros(d.savings)
        );
        v_flex()
            .gap_4()
            .p_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        ui::card("Cost by model", cx)
                            .flex_1()
                            .min_w(px(300.))
                            .child(legend_of(&d.cost.series))
                            .child(
                                bar_chart(d.cost.clone(), self.hovers.cost.clone(), theme)
                                    .w_full()
                                    .h(px(240.)),
                            ),
                    )
                    .child(
                        ui::card("Tokens by model", cx)
                            .flex_1()
                            .min_w(px(300.))
                            .child(legend_of(&d.tokens.series))
                            .child(
                                line_chart(d.tokens.clone(), self.hovers.tokens.clone(), theme)
                                    .w_full()
                                    .h(px(240.)),
                            ),
                    ),
            )
            .child(
                ui::card("Models in range", cx)
                    .child(ui::muted_text(summary, cx))
                    .child(self.render_table(d, cx))
                    .child(ui::muted_text(
                        "$/Mtok = cost ÷ tokens of that kind × 1M, so the prompt and output rates are all-in costs per token, not list prices. Cache savings: what cache reads saved compared with paying the input rate.",
                        cx,
                    )),
            )
            .into_any_element()
    }
}

impl Render for ModelsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(d) if d.no_data => ui::status_message(NO_DATA_MESSAGE, cx),
            Loadable::Ready(d) if d.rows.is_empty() => ui::status_message("No usage in range", cx),
            Loadable::Ready(d) => self.render_ready(d, cx),
        };
        ui::page_scroll("models-scroll", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::MockData;
    use jiff::Timestamp;
    use tbt_core::TimeRange;
    use tbt_store::{BucketSize, TokenSums};

    fn stats(model: &str, cost: i64, tokens: TokenSums, unpriced: u64) -> ModelStats {
        ModelStats {
            provider: "anthropic".into(),
            model: model.into(),
            events: 3,
            tokens,
            cost: Micros(cost),
            unpriced_events: unpriced,
            cache_savings: Micros::ZERO,
        }
    }

    #[test]
    fn effective_rates_per_mtok() {
        let m = stats(
            "m",
            6_000_000,
            TokenSums {
                input: 500_000,
                output: 500_000,
                cache_write: 0,
                cache_read: 1_000_000,
            },
            0,
        );
        let r = rates(&m);
        assert_eq!(r.total, Some(3.0));
        assert_eq!(r.prompt, Some(4.0));
        assert_eq!(r.output, Some(12.0));
    }

    #[test]
    fn effective_rates_with_zero_tokens_or_unpriced() {
        let m = stats("m", 0, TokenSums::default(), 0);
        assert_eq!(rates(&m), Rates::default());
        let only_input = stats(
            "m",
            1_000_000,
            TokenSums {
                input: 1_000_000,
                ..Default::default()
            },
            0,
        );
        let r = rates(&only_input);
        assert_eq!(r.total, Some(1.0));
        assert_eq!(r.output, None);
        let unpriced = stats(
            "m",
            0,
            TokenSums {
                input: 10,
                ..Default::default()
            },
            3,
        );
        assert_eq!(rates(&unpriced), Rates::default());
        assert_eq!(per_mtok(Micros(1), 0), None);
    }

    #[test]
    fn ranking_puts_unpriced_models_after_priced_by_tokens() {
        let t = |n| TokenSums {
            input: n,
            ..Default::default()
        };
        let ranked = rank_models(vec![
            stats("small", 0, t(10), 3),
            stats("cheap", 5, t(1), 0),
            stats("big", 0, t(99), 3),
            stats("dear", 50, t(1), 0),
        ]);
        let names: Vec<&str> = ranked.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(names, ["dear", "cheap", "big", "small"]);
    }

    #[test]
    fn top_models_keep_order_and_rest_is_grouped() {
        let order: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let pivoted = BTreeMap::from([
            ("c".to_owned(), vec![1.0, 0.0]),
            ("a".to_owned(), vec![2.0, 2.0]),
            ("b".to_owned(), vec![0.0, 3.0]),
            ("z".to_owned(), vec![4.0, 1.0]),
        ]);
        let g = group_top(&order, pivoted.clone(), 2);
        assert_eq!(
            g,
            vec![
                ("a".to_owned(), vec![2.0, 2.0]),
                ("b".to_owned(), vec![0.0, 3.0]),
                (OTHER.to_owned(), vec![5.0, 1.0]),
            ]
        );
        let all = group_top(&order, pivoted, 10);
        assert_eq!(all.len(), 4);
        assert_eq!(all[3].0, OTHER);
        assert!(group_top(&order, BTreeMap::new(), 2).is_empty());
    }

    #[test]
    fn load_matches_model_stats() {
        let now: Timestamp = "2026-11-23T15:20:00Z".parse().unwrap();
        let data = MockData::new(11, now);
        let filter = QueryFilter::new(
            TimeRange::new(
                "2026-11-17T00:00:00Z".parse().unwrap(),
                "2026-11-24T00:00:00Z".parse().unwrap(),
            ),
            BucketSize::Day,
        );
        let d = ModelsData::load(&data, &filter).unwrap();
        assert!(!d.no_data);
        assert_eq!(d.cost.labels.len(), 7);
        let charted: f64 = d.cost.series.iter().flat_map(|s| s.values.iter()).sum();
        assert!((charted - micros_to_usd(d.cost_total)).abs() < 1e-6);
        let tokens: f64 = d.tokens.series.iter().flat_map(|s| s.values.iter()).sum();
        let table_tokens: u64 = d.rows.iter().map(|r| r.stats.tokens.total()).sum();
        assert_eq!(tokens as u64, table_tokens);
        assert!(d.cost.series.len() <= TOP_MODELS + 1);
        assert!(
            d.rows
                .windows(2)
                .all(|w| w[0].stats.cost >= w[1].stats.cost)
        );
    }
}
