use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tbt_core::{Micros, SessionId, TokenCounts};
use tbt_store::{EventRow, MessageRow, SessionDetail, SessionRow};

use super::sessions::wall_secs;
use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_count, fmt_duration, fmt_micros, fmt_pct, fmt_tokens, fmt_usd};
use crate::charts::timeline::{CallTimeline, TipRow, call_timeline};
use crate::charts::{ChartTheme, Hover, Series, legend};
use crate::data::DataSource;
use crate::palette::{TokenKind, categorical, harness_color, harness_label};
use crate::ui::{self, Col};

pub const METADATA_ONLY_NOTE: &str = "Message text not stored (privacy: metadata only)";
pub const NO_MESSAGES_NOTE: &str =
    "No messages stored for this session (message storage is off, or the harness logs none)";

const MESSAGE_ROW_HEIGHT: f32 = 26.0;

pub fn debug_open_session() -> Option<SessionId> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("TBT_OPEN_SESSION")
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(SessionId)
}

pub fn debug_open_from_env(app_state: &Entity<AppState>, cx: &mut App) {
    if let Some(id) = debug_open_session() {
        let app_state = app_state.clone();
        cx.defer(move |cx| app_state.update(cx, |s, cx| s.open_session(id, cx)));
    }
}

pub fn cumulative_cost(events: &[EventRow]) -> Vec<f64> {
    events
        .iter()
        .scan(Micros::ZERO, |acc, e| {
            *acc += e.cost;
            Some(crate::charts::format::micros_to_usd(*acc))
        })
        .collect()
}

pub fn call_token_series(events: &[EventRow]) -> Vec<Vec<f64>> {
    TokenKind::ALL
        .iter()
        .map(|k| {
            events
                .iter()
                .map(|e| k.of(&e.tokens.into()) as f64)
                .collect()
        })
        .collect()
}

pub fn token_totals(events: &[EventRow]) -> TokenCounts {
    events.iter().map(|e| e.tokens).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostCompare {
    pub computed: Micros,
    pub reported: Option<Micros>,
    pub delta: Option<Micros>,
    pub api_equivalent: bool,
}

impl CostCompare {
    pub fn new(computed: Micros, reported: Option<Micros>) -> CostCompare {
        let reported = reported.filter(|r| *r != Micros::ZERO);
        CostCompare {
            computed,
            reported,
            delta: reported.map(|r| r - computed),
            api_equivalent: reported.is_none(),
        }
    }

    pub fn label(&self) -> Option<String> {
        let (reported, delta) = (self.reported?, self.delta?);
        Some(format!(
            "reported {} · computed {} · Δ {}",
            fmt_micros(reported),
            fmt_micros(self.computed),
            fmt_delta(delta, self.computed)
        ))
    }
}

pub fn fmt_delta(delta: Micros, base: Micros) -> String {
    let amount = if delta >= Micros::ZERO {
        format!("+{}", fmt_micros(delta))
    } else {
        fmt_micros(delta)
    };
    if base > Micros::ZERO {
        let pct = delta.0 as f64 / base.0 as f64;
        let sign = if pct >= 0.0 { "+" } else { "" };
        format!("{amount} ({sign}{})", fmt_pct(pct))
    } else {
        amount
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub links: Vec<SessionRow>,
    pub current: usize,
    pub total: Micros,
}

impl Chain {
    pub fn new(links: Vec<SessionRow>, session: SessionId) -> Chain {
        let current = links.iter().position(|r| r.id == session).unwrap_or(0);
        Chain {
            total: chain_total(&links),
            links,
            current,
        }
    }

    pub fn previous(&self) -> Option<&SessionRow> {
        self.current.checked_sub(1).and_then(|i| self.links.get(i))
    }

    pub fn next(&self) -> Option<&SessionRow> {
        self.links.get(self.current + 1)
    }

    pub fn is_chain(&self) -> bool {
        self.links.len() > 1
    }
}

pub fn chain_total(links: &[SessionRow]) -> Micros {
    links.iter().map(|r| r.cost).sum()
}

pub fn message_note(messages: &[MessageRow]) -> Option<&'static str> {
    if messages.is_empty() {
        Some(NO_MESSAGES_NOTE)
    } else if messages.iter().all(|m| m.preview.is_none()) {
        Some(METADATA_ONLY_NOTE)
    } else {
        None
    }
}

pub fn session_name(row: &SessionRow) -> String {
    match row.title.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => {
            let short: String = row.ext_id.chars().take(8).collect();
            format!("Session {short}")
        }
    }
}

pub fn span_label(row: &SessionRow) -> String {
    let wall = wall_secs(row);
    let active = row.active_seconds;
    if wall == 0 {
        return format!("{} active", fmt_duration(active));
    }
    format!(
        "{} wall · {} active ({})",
        fmt_duration(wall),
        fmt_duration(active),
        fmt_pct(active.min(wall) as f64 / wall as f64)
    )
}

fn spans_days(events: &[EventRow], tz: &TimeZone) -> bool {
    match (events.first(), events.last()) {
        (Some(a), Some(b)) => {
            a.ts.to_zoned_datetime(tz.clone()).date() != b.ts.to_zoned_datetime(tz.clone()).date()
        }
        _ => false,
    }
}

fn fmt_time(ts: Timestamp, tz: &TimeZone, with_date: bool) -> String {
    let fmt = if with_date {
        "%b %-d, %H:%M:%S"
    } else {
        "%H:%M:%S"
    };
    ts.to_zoned_datetime(tz.clone()).strftime(fmt).to_string()
}

fn fmt_stamp(ts: Timestamp, tz: &TimeZone) -> String {
    ts.to_zoned_datetime(tz.clone())
        .strftime("%a %b %-d %Y, %H:%M")
        .to_string()
}

pub fn call_tooltip(e: &EventRow, cumulative: f64) -> Vec<TipRow> {
    let mut rows = vec![TipRow::new("Model", e.model.clone())];
    let sums: tbt_store::TokenSums = e.tokens.into();
    for kind in TokenKind::ALL {
        let v = kind.of(&sums);
        if v > 0 {
            rows.push(TipRow {
                swatch: Some(kind.color()),
                label: kind.label().into(),
                value: fmt_tokens(v).into(),
            });
        }
    }
    if e.tokens.reasoning > 0 {
        rows.push(TipRow::new(
            "Reasoning (in output)",
            fmt_tokens(e.tokens.reasoning),
        ));
    }
    rows.push(TipRow::new("Cost", fmt_micros(e.cost)));
    if let Some(r) = e.reported_cost {
        rows.push(TipRow::new("Reported", fmt_micros(r)));
    }
    rows.push(TipRow::new("Cumulative", fmt_usd(cumulative)));
    rows
}

pub fn build_timeline(events: &[EventRow], tz: &TimeZone) -> CallTimeline {
    let with_date = spans_days(events, tz);
    let cum = cumulative_cost(events);
    CallTimeline {
        labels: (1..=events.len()).map(|i| i.to_string().into()).collect(),
        series: TokenKind::ALL
            .iter()
            .zip(call_token_series(events))
            .map(|(k, values)| Series {
                name: k.label().into(),
                color: k.color(),
                values,
            })
            .collect(),
        titles: events
            .iter()
            .enumerate()
            .map(|(i, e)| format!("Call {} · {}", i + 1, fmt_time(e.ts, tz, with_date)).into())
            .collect(),
        tooltips: events
            .iter()
            .zip(&cum)
            .map(|(e, c)| call_tooltip(e, *c))
            .collect(),
        cumulative: Series {
            name: "Cumulative cost".into(),
            color: categorical(4),
            values: cum,
        },
    }
}

pub struct MessageLine {
    pub time: SharedString,
    pub role: SharedString,
    pub kind: SharedString,
    pub tool: SharedString,
    pub preview: Option<SharedString>,
}

impl MessageLine {
    fn new(m: &MessageRow, tz: &TimeZone, with_date: bool) -> MessageLine {
        MessageLine {
            time: fmt_time(m.ts, tz, with_date).into(),
            role: m.role.as_str().into(),
            kind: m
                .kind
                .map(|k| k.as_str().replace('_', " "))
                .unwrap_or_default()
                .into(),
            tool: m.tool_name.clone().unwrap_or_default().into(),
            preview: m
                .preview
                .as_ref()
                .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ").into()),
        }
    }
}

pub struct DetailData {
    pub detail: SessionDetail,
    pub parent: Option<SessionRow>,
    pub chain: Chain,
    pub tokens: TokenCounts,
    pub compare: CostCompare,
    pub timeline: CallTimeline,
    pub messages: Vec<MessageLine>,
    pub note: Option<&'static str>,
    pub tz: TimeZone,
}

impl DetailData {
    pub fn load(
        data: &dyn DataSource,
        id: SessionId,
        tz: TimeZone,
    ) -> anyhow::Result<Option<DetailData>> {
        let Some(detail) = data.session_detail(id)? else {
            return Ok(None);
        };
        let parent = match detail.session.parent {
            Some(p) => data.session_row(p)?,
            None => None,
        };
        Ok(Some(DetailData::build(detail, parent, tz)))
    }

    pub fn build(detail: SessionDetail, parent: Option<SessionRow>, tz: TimeZone) -> DetailData {
        let chain = Chain::new(detail.resume_chain.clone(), detail.session.id);
        let with_date = spans_days(&detail.events, &tz);
        DetailData {
            parent,
            chain,
            tokens: token_totals(&detail.events),
            compare: CostCompare::new(detail.session.cost, detail.reported_cost),
            timeline: build_timeline(&detail.events, &tz),
            messages: detail
                .messages
                .iter()
                .map(|m| MessageLine::new(m, &tz, with_date))
                .collect(),
            note: message_note(&detail.messages),
            detail,
            tz,
        }
    }

    pub fn subagents_total(&self) -> Micros {
        chain_total(&self.detail.subagents)
    }
}

pub struct SessionDetailPage {
    pub session: SessionId,
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<Option<Arc<DetailData>>>,
    hover: Hover,
    messages_scroll: UniformListScrollHandle,
    _load: Task<()>,
    _subscription: Subscription,
}

impl SessionDetailPage {
    pub fn new(
        session: SessionId,
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&app_state, |this, _, ev: &AppStateEvent, cx| {
            if matches!(ev, AppStateEvent::DataChanged) {
                this.reload(cx);
            }
        });
        let mut page = SessionDetailPage {
            session,
            app_state,
            data,
            state: Loadable::Loading,
            hover: Hover::default(),
            messages_scroll: UniformListScrollHandle::new(),
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let tz = self.app_state.read(cx).tz();
        let (data, id) = (self.data.clone(), self.session);
        self._load = spawn_load(
            cx,
            move || Ok(DetailData::load(&*data, id, tz)?.map(Arc::new)),
            |this, state, _| this.state = state,
        );
    }

    fn open(&self, id: SessionId) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let app_state = self.app_state.clone();
        move |_, _, cx| app_state.update(cx, |s, cx| s.open_session(id, cx))
    }

    fn render_header(&self, d: &DetailData, cx: &App) -> impl IntoElement {
        let t = cx.theme();
        let s = &d.detail.session;
        let tok = &d.tokens;
        let mut meta: Vec<String> = Vec::new();
        meta.push(
            s.project_name
                .clone()
                .or_else(|| s.project_path.clone())
                .unwrap_or_else(|| "(no project)".into()),
        );
        if let Some(b) = &s.git_branch {
            meta.push(format!("⎇ {b}"));
        }
        if let Some(v) = &d.detail.harness_version {
            meta.push(format!("v{v}"));
        }
        if !s.models.is_empty() {
            meta.push(s.models.join(", "));
        }
        let ended = s
            .ended_at
            .map(|e| fmt_stamp(e, &d.tz))
            .unwrap_or_else(|| "–".into());
        let cost_value = fmt_micros(s.cost);
        let cost_sub = match d.compare.label() {
            Some(l) => l,
            None => "API-equivalent cost".to_owned(),
        };
        let mut token_sub = format!(
            "in {} · out {} · cache write {} · cache read {}",
            fmt_tokens(tok.input),
            fmt_tokens(tok.output),
            fmt_tokens(tok.cache_write()),
            fmt_tokens(tok.cache_read)
        );
        if tok.reasoning > 0 {
            token_sub.push_str(&format!(" · reasoning {}", fmt_tokens(tok.reasoning)));
        }
        ui::card(session_name(s), cx)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(
                        div()
                            .size(px(9.))
                            .rounded_full()
                            .bg(harness_color(&s.harness)),
                    )
                    .child(harness_label(&s.harness))
                    .child(
                        div()
                            .text_color(t.muted_foreground)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(meta.join("  ·  ")),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_8()
                    .gap_y_3()
                    .items_start()
                    .child(stat(
                        "Started",
                        fmt_stamp(s.started_at, &d.tz).into_any_element(),
                        Some(format!("ended {ended}")),
                        cx,
                    ))
                    .child(stat(
                        "Duration",
                        fmt_duration(wall_secs(s).max(s.active_seconds)).into_any_element(),
                        Some(span_label(s)),
                        cx,
                    ))
                    .child(stat(
                        "Calls",
                        fmt_count(s.events).into_any_element(),
                        Some(format!("{} tool uses", fmt_count(tool_uses(&d.detail)))),
                        cx,
                    ))
                    .child(stat(
                        "Tokens",
                        fmt_tokens(tok.total()).into_any_element(),
                        Some(token_sub),
                        cx,
                    ))
                    .child(stat(
                        "Cost",
                        cost_value.into_any_element(),
                        Some(cost_sub),
                        cx,
                    )),
            )
            .when(d.compare.reported.is_some(), |c| {
                c.child(ui::muted_text(
                    "Computed cost uses list API prices; the reported cost is what the harness logged.",
                    cx,
                ))
            })
    }

    fn render_timeline(&self, d: &DetailData, cx: &App) -> AnyElement {
        let t = cx.theme();
        let card = ui::card("Calls", cx);
        if d.detail.events.is_empty() {
            return card
                .child(ui::muted_text("No API calls recorded", cx))
                .into_any_element();
        }
        let items = d
            .timeline
            .series
            .iter()
            .map(|s| (s.name.clone(), s.color))
            .chain([(
                d.timeline.cumulative.name.clone(),
                d.timeline.cumulative.color,
            )]);
        card.child(
            h_flex()
                .justify_between()
                .child(legend(items, t.muted_foreground))
                .child(ui::muted_text(
                    "Tokens per call (left) · cumulative cost (right)",
                    cx,
                )),
        )
        .child(
            call_timeline(
                d.timeline.clone(),
                self.hover.clone(),
                ChartTheme::from_cx(cx),
            )
            .w_full()
            .h(px(240.)),
        )
        .into_any_element()
    }

    fn render_tools(&self, d: &DetailData, cx: &App) -> AnyElement {
        let t = cx.theme();
        let card = ui::card("Tool use", cx).flex_1().min_w(px(300.));
        let tools = &d.detail.tool_counts;
        if tools.is_empty() {
            let text = if d.detail.messages.is_empty() {
                "Tool counts come from stored messages; none are stored"
            } else {
                "No tool calls"
            };
            return card.child(ui::muted_text(text, cx)).into_any_element();
        }
        let max = tools.first().map(|(_, n)| *n).unwrap_or(1).max(1) as f64;
        let cols = [
            Col::flex("Tool"),
            Col::num("Calls", 60.0),
            Col::fixed("", 140.0),
        ];
        let (accent, track) = (categorical(0), t.border.opacity(0.4));
        let rows = tools
            .iter()
            .map(|(name, n)| {
                vec![
                    name.clone().into_any_element(),
                    fmt_count(*n).into_any_element(),
                    ui::bar_cell(*n as f64 / max, accent, track).into_any_element(),
                ]
            })
            .collect();
        card.child(ui::table(&cols, rows, cx)).into_any_element()
    }

    fn link(
        &self,
        key: &str,
        row: &SessionRow,
        current: bool,
        d: &DetailData,
        cx: &App,
    ) -> AnyElement {
        let t = cx.theme();
        let (hover_bg, cur_bg, link, muted) = (
            t.foreground.opacity(0.05),
            t.secondary_active,
            t.link,
            t.muted_foreground,
        );
        h_flex()
            .id(SharedString::from(format!("{key}-{}", row.id.0)))
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .items_center()
            .when(current, |e| e.bg(cur_bg))
            .when(!current, |e| {
                e.cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(self.open(row.id))
            })
            .child(ui::swatch(harness_color(&row.harness)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(if current { t.foreground } else { link })
                    .child(session_name(row)),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(fmt_stamp(row.started_at, &d.tz)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(110.))
                    .flex()
                    .justify_end()
                    .child(fmt_micros(row.cost)),
            )
            .into_any_element()
    }

    fn section(title: &'static str, cx: &App) -> gpui::Div {
        div()
            .pt_1()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(title)
    }

    fn render_related(&self, d: &DetailData, cx: &App) -> AnyElement {
        let mut card = ui::card("Related sessions", cx).flex_1().min_w(px(340.));
        let mut any = false;
        if let Some(p) = &d.parent {
            any = true;
            card = card
                .child(Self::section("Subagent of", cx))
                .child(self.link("parent", p, false, d, cx));
        } else if let Some(pid) = d.detail.session.parent {
            any = true;
            card = card
                .child(Self::section("Subagent of", cx))
                .child(ui::muted_text(
                    format!("Session #{} (not found)", pid.0),
                    cx,
                ));
        }
        if d.chain.is_chain() {
            any = true;
            let c = &d.chain;
            let nav = h_flex()
                .gap_2()
                .when_some(c.previous(), |e, p| {
                    e.child(
                        Button::new("chain-prev")
                            .ghost()
                            .label("← Previous")
                            .on_click(self.open(p.id)),
                    )
                })
                .when_some(c.next(), |e, n| {
                    e.child(
                        Button::new("chain-next")
                            .ghost()
                            .label("Next →")
                            .on_click(self.open(n.id)),
                    )
                });
            card = card
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(Self::section("Resume chain", cx))
                        .child(nav),
                )
                .children(
                    c.links
                        .iter()
                        .enumerate()
                        .map(|(i, r)| self.link("chain", r, i == c.current, d, cx)),
                )
                .child(ui::muted_text(
                    format!(
                        "Combined: {} across {} sessions",
                        fmt_micros(c.total),
                        c.links.len()
                    ),
                    cx,
                ));
        }
        let subs = &d.detail.subagents;
        if !subs.is_empty() {
            any = true;
            card = card
                .child(Self::section("Subagents", cx))
                .children(subs.iter().map(|r| self.link("sub", r, false, d, cx)))
                .child(ui::muted_text(
                    format!(
                        "{} subagents · {}",
                        subs.len(),
                        fmt_micros(d.subagents_total())
                    ),
                    cx,
                ));
        }
        if !any {
            card = card.child(ui::muted_text("Not resumed, no subagents", cx));
        }
        card.into_any_element()
    }

    fn render_message_row(&self, ix: usize, cx: &App) -> AnyElement {
        let t = cx.theme();
        let Loadable::Ready(Some(d)) = &self.state else {
            return div().into_any_element();
        };
        let Some(m) = d.messages.get(ix) else {
            return div().into_any_element();
        };
        let muted = t.muted_foreground;
        h_flex()
            .w_full()
            .h(px(MESSAGE_ROW_HEIGHT))
            .gap_3()
            .px_2()
            .text_sm()
            .border_b_1()
            .border_color(t.border.opacity(0.3))
            .child(MSG_COLS[0].cell(div().text_color(muted).child(m.time.clone())))
            .child(MSG_COLS[1].cell(m.role.clone()))
            .child(MSG_COLS[2].cell(div().text_color(muted).child(m.kind.clone())))
            .child(MSG_COLS[3].cell(m.tool.clone()))
            .child(MSG_COLS[4].cell(match &m.preview {
                Some(p) => div().child(p.clone()),
                None => div().text_color(muted.opacity(0.6)).child("—"),
            }))
            .into_any_element()
    }

    fn render_messages(&self, d: &DetailData, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, border) = (t.muted_foreground, t.border);
        let title = format!("Messages ({})", fmt_count(d.messages.len() as u64));
        let card = ui::card(title, cx).children(d.note.map(|n| ui::muted_text(n, cx)));
        if d.messages.is_empty() {
            return card.into_any_element();
        }
        let header = h_flex()
            .gap_3()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .children(MSG_COLS.iter().map(|c| c.cell(c.title)));
        let height = (d.messages.len() as f32 * MESSAGE_ROW_HEIGHT).min(360.0);
        card.child(
            v_flex().child(header).child(
                div().h(px(height)).child(
                    uniform_list(
                        "session-messages",
                        d.messages.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            range.map(|ix| this.render_message_row(ix, cx)).collect()
                        }),
                    )
                    .track_scroll(&self.messages_scroll)
                    .size_full(),
                ),
            ),
        )
        .into_any_element()
    }

    fn render_ready(&self, d: &DetailData, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_4()
            .p_4()
            .pt_0()
            .child(self.render_header(d, cx))
            .child(self.render_timeline(d, cx))
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .flex_wrap()
                    .child(self.render_tools(d, cx))
                    .child(self.render_related(d, cx)),
            )
            .child(self.render_messages(d, cx))
            .into_any_element()
    }
}

const MSG_COLS: [Col; 5] = [
    Col::fixed("Time", 120.0),
    Col::fixed("Role", 72.0),
    Col::fixed("Kind", 84.0),
    Col::fixed("Tool", 120.0),
    Col::flex("Preview"),
];

fn tool_uses(d: &SessionDetail) -> u64 {
    d.tool_counts.iter().map(|(_, n)| n).sum()
}

fn stat(label: &'static str, value: AnyElement, sub: Option<String>, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    v_flex()
        .gap_0p5()
        .min_w(px(120.))
        .child(div().text_xs().text_color(t.muted_foreground).child(label))
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(t.foreground)
                .child(value),
        )
        .children(sub.map(|s| div().text_xs().text_color(t.muted_foreground).child(s)))
}

impl Render for SessionDetailPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app_state = self.app_state.clone();
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load session: {e}"), cx),
            Loadable::Ready(None) => {
                ui::status_message(format!("Session #{} not found", self.session.0), cx)
            }
            Loadable::Ready(Some(d)) => {
                let d = d.clone();
                self.render_ready(&d, cx)
            }
        };
        v_flex()
            .size_full()
            .child(
                h_flex().flex_none().px_4().py_2().child(
                    Button::new("session-back")
                        .ghost()
                        .label("← Back")
                        .on_click(move |_, _, cx| {
                            app_state.update(cx, |s, cx| s.close_session(cx))
                        }),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(ui::page_scroll("session-detail-scroll", body)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::{EventId, HarnessId, MessageKind, MessageRole, PriceId};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn event(i: i64, cost: i64, input: u64, output: u64, cache_read: u64) -> EventRow {
        EventRow {
            id: EventId(i),
            ts: ts("2026-11-20T10:00:00Z") + jiff::SignedDuration::from_secs(i * 30),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            tokens: TokenCounts {
                input,
                output,
                cache_write_5m: 10,
                cache_write_1h: 5,
                cache_read,
                reasoning: output / 2,
            },
            cost: Micros(cost),
            reported_cost: None,
            price_id: Some(PriceId(1)),
        }
    }

    fn row(id: i64, cost: i64, resumed_from: Option<i64>, start: &str) -> SessionRow {
        SessionRow {
            id: SessionId(id),
            harness: HarnessId::CLAUDE_CODE,
            ext_id: format!("abcdef0123-{id}"),
            title: None,
            project_path: None,
            project_name: None,
            git_branch: None,
            models: vec![],
            started_at: ts(start),
            ended_at: None,
            active_seconds: 0,
            events: 1,
            tokens: Default::default(),
            cost: Micros(cost),
            unpriced_events: 0,
            parent: None,
            resumed_from: resumed_from.map(SessionId),
        }
    }

    #[test]
    fn cumulative_cost_is_a_running_total_in_usd() {
        let events = [
            event(1, 1_500_000, 1, 1, 0),
            event(2, 0, 1, 1, 0),
            event(3, 250_000, 1, 1, 0),
        ];
        assert_eq!(cumulative_cost(&events), vec![1.5, 1.5, 1.75]);
        assert!(cumulative_cost(&[]).is_empty());
    }

    #[test]
    fn per_call_stacks_by_token_type() {
        let events = [event(1, 0, 100, 20, 1000), event(2, 0, 0, 7, 50)];
        let s = call_token_series(&events);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], vec![100.0, 0.0]);
        assert_eq!(s[1], vec![20.0, 7.0]);
        assert_eq!(s[2], vec![15.0, 15.0]);
        assert_eq!(s[3], vec![1000.0, 50.0]);
        let tops = crate::charts::stack::totals(&s);
        for (top, e) in tops.iter().zip(&events) {
            assert_eq!(*top as u64, e.tokens.total());
        }
        let t = token_totals(&events);
        assert_eq!(
            t.total(),
            events.iter().map(|e| e.tokens.total()).sum::<u64>()
        );
        assert_eq!(t.reasoning, 13);
    }

    #[test]
    fn reported_vs_computed_delta() {
        let c = CostCompare::new(Micros(2_000_000), Some(Micros(2_500_000)));
        assert_eq!(c.delta, Some(Micros(500_000)));
        assert!(!c.api_equivalent);
        assert_eq!(
            c.label().unwrap(),
            "reported $2.50 · computed $2.00 · Δ +$0.50 (+25.0%)"
        );
        let c = CostCompare::new(Micros(2_000_000), Some(Micros(1_000_000)));
        assert_eq!(c.delta, Some(Micros(-1_000_000)));
        assert_eq!(fmt_delta(c.delta.unwrap(), c.computed), "-$1.00 (-50.0%)");
        assert_eq!(fmt_delta(Micros(10_000), Micros::ZERO), "+$0.01");
    }

    #[test]
    fn missing_or_zero_reported_cost_is_api_equivalent() {
        for reported in [None, Some(Micros::ZERO)] {
            let c = CostCompare::new(Micros(1_000_000), reported);
            assert!(c.api_equivalent);
            assert_eq!(c.reported, None);
            assert_eq!(c.delta, None);
            assert_eq!(c.label(), None);
        }
    }

    #[test]
    fn chain_combines_backward_and_forward_links() {
        let a = row(1, 1_000_000, None, "2026-11-20T10:00:00Z");
        let b = row(2, 2_000_000, Some(1), "2026-11-20T12:00:00Z");
        let c = row(3, 500_000, Some(2), "2026-11-21T09:00:00Z");
        let fork = row(4, 9_000_000, Some(2), "2026-11-22T09:00:00Z");
        let chain = Chain::new(vec![a.clone(), b.clone(), c.clone(), fork], b.id);
        assert_eq!(chain.current, 1);
        assert_eq!(chain.total, Micros(12_500_000));
        assert_eq!(chain.previous().map(|r| r.id), Some(a.id));
        assert_eq!(chain.next().map(|r| r.id), Some(c.id));
        assert!(chain.is_chain());

        let alone = Chain::new(vec![a.clone()], a.id);
        assert!(!alone.is_chain());
        assert_eq!(alone.total, a.cost);
        assert_eq!(alone.previous(), None);
        assert_eq!(alone.next(), None);
        assert_eq!(chain_total(&[]), Micros::ZERO);
    }

    #[test]
    fn message_privacy_notes() {
        let m = |preview: Option<&str>| MessageRow {
            ts: ts("2026-11-20T10:00:00Z"),
            role: MessageRole::Assistant,
            kind: Some(MessageKind::ToolUse),
            tool_name: Some("Read".into()),
            preview: preview.map(str::to_owned),
            event: None,
        };
        assert_eq!(message_note(&[]), Some(NO_MESSAGES_NOTE));
        assert_eq!(message_note(&[m(None), m(None)]), Some(METADATA_ONLY_NOTE));
        assert_eq!(message_note(&[m(None), m(Some("hi"))]), None);
        let line = MessageLine::new(&m(Some("a\n  b")), &TimeZone::UTC, false);
        assert_eq!(line.time.as_ref(), "10:00:00");
        assert_eq!(line.kind.as_ref(), "tool use");
        assert_eq!(line.preview.as_ref().map(|p| p.as_ref()), Some("a b"));
    }

    #[test]
    fn tooltip_counts_unpriced_calls_as_zero() {
        let mut e = event(1, 0, 10, 0, 0);
        e.price_id = None;
        let rows = call_tooltip(&e, 0.0);
        let cost = rows.iter().find(|r| r.label.as_ref() == "Cost").unwrap();
        assert_eq!(cost.value.as_ref(), "$0.00");
        assert!(rows.iter().all(|r| r.label.as_ref() != "Output"));
        let priced = call_tooltip(&event(1, 1_230_000, 10, 4, 0), 2.0);
        let labels: Vec<&str> = priced.iter().map(|r| r.label.as_ref()).collect();
        assert_eq!(
            labels,
            [
                "Model",
                "Input",
                "Output",
                "Cache write",
                "Reasoning (in output)",
                "Cost",
                "Cumulative"
            ]
        );
        assert_eq!(priced[5].value.as_ref(), "$1.23");
    }

    #[test]
    fn timeline_labels_and_titles() {
        let events = [event(1, 100, 1, 1, 0), event(2, 200, 1, 1, 0)];
        let t = build_timeline(&events, &TimeZone::UTC);
        assert_eq!(t.labels, vec![SharedString::from("1"), "2".into()]);
        assert_eq!(t.titles[1].as_ref(), "Call 2 · 10:01:00");
        assert_eq!(t.series.len(), 4);
        assert_eq!(t.tooltips.len(), 2);
        assert_eq!(t.cumulative.values.len(), 2);
    }

    #[test]
    fn span_and_name_labels() {
        let mut r = row(1, 0, None, "2026-11-20T10:00:00Z");
        r.ended_at = Some(ts("2026-11-20T12:00:00Z"));
        r.active_seconds = 1800;
        assert_eq!(span_label(&r), "2h 0m wall · 30m active (25.0%)");
        assert_eq!(session_name(&r), "Session abcdef01");
        r.title = Some("  Fix the parser ".into());
        assert_eq!(session_name(&r), "Fix the parser");
        r.ended_at = None;
        assert_eq!(span_label(&r), "30m active");
    }
}
