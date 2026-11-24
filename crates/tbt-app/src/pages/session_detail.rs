use std::sync::Arc;

use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::v_flex;
use tbt_core::SessionId;

use crate::app_state::AppState;
use crate::data::DataSource;

pub struct SessionDetailPage {
    pub session: SessionId,
    app_state: Entity<AppState>,
    _data: Arc<dyn DataSource>,
}

impl SessionDetailPage {
    pub fn new(
        session: SessionId,
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        SessionDetailPage {
            session,
            app_state,
            _data: data,
        }
    }
}

impl Render for SessionDetailPage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let app_state = self.app_state.clone();
        v_flex()
            .p_4()
            .gap_2()
            .child(
                Button::new("session-back")
                    .ghost()
                    .label("← Back")
                    .on_click(move |_, _, cx| app_state.update(cx, |s, cx| s.close_session(cx))),
            )
            .child(div().child(format!("Session #{}", self.session.0)))
    }
}
