use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use jiff::Timestamp;
use tbt_store::{HarnessStatus, IngestRunSummary};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent, relative_time};
use crate::charts::format::fmt_count;
use crate::data::DataSource;
use crate::palette::harness_color;
use crate::ui::{self, Col};

pub struct HarnessesData {
    pub status: Vec<HarnessStatus>,
    pub runs: Vec<IngestRunSummary>,
}

pub struct HarnessesPage {
    data: Arc<dyn DataSource>,
    state: Loadable<HarnessesData>,
    _load: Task<()>,
    _subscription: Subscription,
}

impl HarnessesPage {
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
        let mut page = HarnessesPage {
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
            move || {
                Ok(HarnessesData {
                    status: data.harness_status()?,
                    runs: data.ingest_runs(12)?,
                })
            },
            |this, state, _| this.state = state,
        );
    }

    fn render_ready(&self, d: &HarnessesData, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (fg, muted, warn) = (t.foreground, t.muted_foreground, t.warning);
        let now = Timestamp::now();
        let cards = d.status.iter().map(|h| {
            let stats = format!(
                "{} files tracked · {} missing · {} sessions · {} calls · last read {}",
                h.files_tracked,
                h.files_missing,
                fmt_count(h.sessions),
                fmt_count(h.events),
                h.last_read
                    .map(|t| relative_time(now, t))
                    .unwrap_or_else(|| "never".into())
            );
            ui::card(h.config.display_name.clone(), cx)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(ui::swatch(harness_color(&h.config.id)))
                        .child(div().text_xs().text_color(fg).child(if h.config.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        }))
                        .child(div().text_xs().text_color(muted).child(stats)),
                )
                .children(h.last_warnings.iter().map(|w| {
                    div()
                        .text_xs()
                        .text_color(warn)
                        .child(format!("⚠ {}: {}", w.source, w.message))
                }))
        });
        let cols = [
            Col::fixed("Started", 150.0),
            Col::fixed("Trigger", 90.0),
            Col::num("Sources", 80.0),
            Col::num("New events", 90.0),
            Col::flex("Result"),
        ];
        let rows = d
            .runs
            .iter()
            .map(|r| {
                vec![
                    relative_time(now, r.started_at).into_any_element(),
                    format!("{:?}", r.trigger).into_any_element(),
                    r.stats.sources_scanned.to_string().into_any_element(),
                    r.stats.events_new.to_string().into_any_element(),
                    r.error
                        .clone()
                        .unwrap_or_else(|| format!("ok, {} warnings", r.warnings.len()))
                        .into_any_element(),
                ]
            })
            .collect();
        v_flex()
            .gap_3()
            .p_4()
            .children(cards)
            .child(ui::card("Recent ingest runs", cx).child(ui::table(&cols, rows, cx)))
            .into_any_element()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(d) => self.render_ready(d, cx),
        };
        ui::page_scroll("harnesses-scroll", body)
    }
}
