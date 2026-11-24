pub mod dashboard;
pub mod harnesses;
pub mod models;
pub mod projects;
pub mod session_detail;
pub mod sessions;
pub mod settings;

use gpui::{Context, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Dashboard,
    Sessions,
    Models,
    Projects,
    Harnesses,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [
        Page::Dashboard,
        Page::Sessions,
        Page::Models,
        Page::Projects,
        Page::Harnesses,
        Page::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => "Dashboard",
            Page::Sessions => "Sessions",
            Page::Models => "Models",
            Page::Projects => "Projects",
            Page::Harnesses => "Harnesses",
            Page::Settings => "Settings",
        }
    }

    pub fn parse(s: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| p.label().eq_ignore_ascii_case(s.trim()))
    }

    pub fn uses_filters(self) -> bool {
        matches!(
            self,
            Page::Dashboard | Page::Sessions | Page::Models | Page::Projects
        )
    }
}

pub enum Loadable<T> {
    Loading,
    Ready(T),
    Failed(String),
}

pub fn spawn_load<P: 'static, T: Send + 'static>(
    cx: &mut Context<P>,
    job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    apply: impl FnOnce(&mut P, Loadable<T>, &mut Context<P>) + 'static,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let result = cx.background_executor().spawn(async move { job() }).await;
        let state = match result {
            Ok(v) => Loadable::Ready(v),
            Err(e) => Loadable::Failed(format!("{e:#}")),
        };
        this.update(cx, |this, cx| {
            apply(this, state, cx);
            cx.notify();
        })
        .ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_parse() {
        assert_eq!(Page::parse("sessions"), Some(Page::Sessions));
        assert_eq!(Page::parse(" Dashboard "), Some(Page::Dashboard));
        assert_eq!(Page::parse("nope"), None);
    }
}
