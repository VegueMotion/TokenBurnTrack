use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, px,
};
use gpui_component::v_flex;
use tbt_store::ModelStats;

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_micros, fmt_tokens, fmt_usd, micros_to_usd};
use crate::charts::{BarChart, ChartTheme, Hover, Series, ValueKind, bar_chart};
use crate::data::DataSource;
use crate::palette::categorical;
use crate::ui::{self, Col};

pub fn usd_per_mtok(m: &ModelStats) -> Option<f64> {
    let tokens = m.tokens.total();
    (tokens > 0).then(|| micros_to_usd(m.cost) / (tokens as f64 / 1e6))
}

pub struct ModelsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<Vec<ModelStats>>,
    hover: Hover,
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
        let subscription =
            cx.subscribe(&app_state, |this, _, _: &AppStateEvent, cx| this.reload(cx));
        let mut page = ModelsPage {
            app_state,
            data,
            state: Loadable::Loading,
            hover: Hover::default(),
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(crate::data::now());
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || Ok(data.model_stats(&filter)?),
            |this, state, _| this.state = state,
        );
    }

    fn render_ready(&self, models: &[ModelStats], cx: &Context<Self>) -> AnyElement {
        let chart = BarChart {
            labels: models.iter().map(|m| m.model.clone().into()).collect(),
            titles: models
                .iter()
                .map(|m| format!("{} / {}", m.provider, m.model).into())
                .collect(),
            series: vec![Series {
                name: "Cost".into(),
                color: categorical(0),
                values: models.iter().map(|m| micros_to_usd(m.cost)).collect(),
            }],
            kind: ValueKind::Usd,
        };
        let cols = [
            Col::flex("Model"),
            Col::fixed("Provider", 90.0),
            Col::num("Calls", 70.0),
            Col::num("Tokens", 80.0),
            Col::num("Cost", 90.0),
            Col::num("$/Mtok", 80.0),
            Col::num("Cache savings", 110.0),
        ];
        let rows = models
            .iter()
            .map(|m| {
                vec![
                    m.model.clone().into_any_element(),
                    m.provider.clone().into_any_element(),
                    fmt_count(m.events).into_any_element(),
                    fmt_tokens(m.tokens.total()).into_any_element(),
                    if m.unpriced_events > 0 {
                        "unpriced".into_any_element()
                    } else {
                        fmt_micros(m.cost).into_any_element()
                    },
                    usd_per_mtok(m)
                        .map(fmt_usd)
                        .unwrap_or_else(|| "–".into())
                        .into_any_element(),
                    fmt_micros(m.cache_savings).into_any_element(),
                ]
            })
            .collect();
        v_flex()
            .gap_4()
            .p_4()
            .child(
                ui::card("Cost per model", cx).child(
                    bar_chart(chart, self.hover.clone(), ChartTheme::from_cx(cx))
                        .w_full()
                        .h(px(220.)),
                ),
            )
            .child(ui::card("Models in range", cx).child(ui::table(&cols, rows, cx)))
            .into_any_element()
    }
}

impl Render for ModelsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(m) if m.is_empty() => ui::status_message("No usage in range", cx),
            Loadable::Ready(m) => self.render_ready(m, cx),
        };
        ui::page_scroll("models-scroll", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::Micros;
    use tbt_store::TokenSums;

    #[test]
    fn effective_price_per_mtok() {
        let mut m = ModelStats {
            provider: "anthropic".into(),
            model: "m".into(),
            events: 1,
            tokens: TokenSums {
                input: 2_000_000,
                ..Default::default()
            },
            cost: Micros(6_000_000),
            unpriced_events: 0,
            cache_savings: Micros::ZERO,
        };
        assert_eq!(usd_per_mtok(&m), Some(3.0));
        m.tokens = TokenSums::default();
        assert_eq!(usd_per_mtok(&m), None);
    }
}
