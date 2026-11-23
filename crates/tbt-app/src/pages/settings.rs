use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window,
};
use gpui_component::v_flex;
use tbt_store::UnpricedModel;

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_tokens};
use crate::data::DataSource;
use crate::ui::{self, Col};

pub struct SettingsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<Vec<UnpricedModel>>,
    _load: Task<()>,
    _subscription: Subscription,
}

impl SettingsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&app_state, |this, _, ev: &AppStateEvent, cx| {
            if *ev == AppStateEvent::DataChanged {
                this.reload(cx);
            }
        });
        let mut page = SettingsPage {
            app_state,
            data,
            state: Loadable::Loading,
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || Ok(data.unpriced_models()?),
            |this, state, _| this.state = state,
        );
    }

    fn render_ready(&self, unpriced: &[UnpricedModel], cx: &Context<Self>) -> AnyElement {
        let tz = self.app_state.read(cx).time_zone.clone();
        let cols = [
            Col::flex("Model"),
            Col::fixed("Provider", 100.0),
            Col::num("Calls", 80.0),
            Col::num("Tokens", 90.0),
            Col::fixed("Last seen", 130.0),
        ];
        let tzv = crate::charts::time::time_zone(&tz);
        let rows = unpriced
            .iter()
            .map(|u| {
                vec![
                    u.model.clone().into_any_element(),
                    u.provider.clone().into_any_element(),
                    fmt_count(u.events).into_any_element(),
                    fmt_tokens(u.tokens.total()).into_any_element(),
                    u.last_seen
                        .to_zoned_datetime(tzv.clone())
                        .strftime("%b %-d, %H:%M")
                        .to_string()
                        .into_any_element(),
                ]
            })
            .collect();
        let general = [
            ("Time zone", tz),
            ("Privacy level", "Metadata only".to_owned()),
            ("Sync interval", "Every hour".to_owned()),
        ];
        v_flex()
            .gap_4()
            .p_4()
            .child(
                ui::card("General", cx).children(
                    general
                        .into_iter()
                        .map(|(k, v)| ui::muted_text(format!("{k}: {v}"), cx)),
                ),
            )
            .child(
                ui::card("Unpriced models", cx)
                    .child(ui::muted_text(
                        "Calls to these models count tokens but no cost until a price is added.",
                        cx,
                    ))
                    .child(if unpriced.is_empty() {
                        ui::muted_text("None", cx).into_any_element()
                    } else {
                        ui::table(&cols, rows, cx).into_any_element()
                    }),
            )
            .into_any_element()
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(u) => self.render_ready(u, cx),
        };
        ui::page_scroll("settings-scroll", body)
    }
}
