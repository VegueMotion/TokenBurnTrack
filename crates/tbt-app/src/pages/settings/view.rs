use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, SharedString, Styled, div, prelude::*,
    px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState, Textarea};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, h_flex, v_flex};
use jiff::tz::TimeZone;
use tbt_core::{ModelPrice, PriceSource};

use super::about;
use super::general::{PRIVACY_OPTIONS, SYNC_MAX_MINUTES, SYNC_MIN_MINUTES};
use super::price_form::{PriceForm, SaveKind, can_close, format_timestamp};
use super::prices::{self, MAX_ROWS, is_closed};
use super::{Loadable, Notice, PricesData, SettingsPage, Tab, value};
use crate::charts::format::{fmt_count, fmt_tokens};
use crate::data;
use crate::export::{Dataset, Format};
use crate::ui::{self, Col};

fn label(text: impl Into<SharedString>, cx: &gpui::App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn field(
    title: &'static str,
    input: &Entity<InputState>,
    width: f32,
    disabled: bool,
    cx: &gpui::App,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .w(px(width))
        .child(label(title, cx))
        .child(Input::new(input).small().disabled(disabled))
}

fn date(ts: Option<jiff::Timestamp>, tz: &TimeZone) -> String {
    ts.map(|t| {
        t.to_zoned_datetime(tz.clone())
            .strftime("%Y-%m-%d")
            .to_string()
    })
    .unwrap_or_else(|| "–".to_owned())
}

fn source_tag(source: PriceSource) -> impl IntoElement {
    let tag = match source {
        PriceSource::User => Tag::primary(),
        PriceSource::Imported => Tag::info(),
        PriceSource::Bundled => Tag::secondary(),
    };
    tag.small().child(prices::source_label(source))
}

impl SettingsPage {
    pub(super) fn render_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = match self.tab {
            Tab::Prices => self.render_prices(cx),
            Tab::Import => self.render_import(cx),
            Tab::General => self.render_general(cx),
            Tab::Adapters => self.render_generic(cx),
            Tab::Export => self.render_export(cx),
            Tab::About => self.render_about(cx),
        };
        v_flex()
            .gap_4()
            .p_4()
            .child(self.render_tabs(cx))
            .children(self.render_notice(cx))
            .child(body)
            .into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .children(Tab::ALL.into_iter().map(|tab| {
                let id = format!("settings-tab-{}", tab.label());
                Button::new(SharedString::from(id.clone()))
                    .debug_selector(move || id)
                    .small()
                    .ghost()
                    .selected(self.tab == tab)
                    .label(tab.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
            }))
            .when(!self.editable(), |d| {
                d.child(div().w(px(12.))).child(label(
                    "Mock data: prices are read-only and nothing is written.",
                    cx,
                ))
            })
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.theme();
        let (text, color) = match self.notice.as_ref()? {
            Notice::Info(s) => (s.clone(), t.foreground),
            Notice::Error(s) => (s.clone(), t.danger),
        };
        Some(
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .bg(t.secondary.opacity(0.5))
                .text_sm()
                .child(div().flex_1().text_color(color).child(text))
                .child(
                    Button::new("notice-dismiss")
                        .xsmall()
                        .ghost()
                        .label("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_prices(&self, cx: &mut Context<Self>) -> AnyElement {
        let data = match &self.prices {
            Loadable::Loading => return ui::muted_text("Loading prices…", cx).into_any_element(),
            Loadable::Failed(e) => {
                return ui::muted_text(format!("Could not load prices: {e}"), cx)
                    .into_any_element();
            }
            Loadable::Ready(d) => d,
        };
        v_flex()
            .gap_4()
            .children(self.form.is_some().then(|| self.render_form(cx)))
            .child(self.render_unpriced(data, cx))
            .child(self.render_price_table(data, cx))
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &self.form else {
            return div().into_any_element();
        };
        let f = &self.form_inputs;
        let tz = self.app_state.read(cx).tz();
        let busy = self.busy || !self.editable();
        let editing_user = state
            .editing
            .as_ref()
            .filter(|p| p.source == PriceSource::User);
        let title = match (&state.editing, &state.unpriced) {
            (Some(p), _) if p.source == PriceSource::User => {
                format!("Edit user price {}/{}", p.provider, p.model_pattern)
            }
            (Some(p), _) => format!(
                "Override {} price {}/{}",
                prices::source_label(p.source),
                p.provider,
                p.model_pattern
            ),
            (None, Some((_, m))) => format!("Add a price for {m}"),
            (None, None) => "Add a user price".to_owned(),
        };
        let from_text = if state.from_now {
            "now".to_owned()
        } else {
            value(&f.from, cx)
        };
        let errors = state.errors.clone();
        let danger = cx.theme().danger;

        let rates = h_flex()
            .gap_3()
            .flex_wrap()
            .child(field("Input $/Mtok", &f.input, 110., false, cx))
            .child(field("Output $/Mtok", &f.output, 110., false, cx))
            .child(field("Cache write 5m", &f.cache_write_5m, 110., false, cx))
            .child(field("Cache write 1h", &f.cache_write_1h, 110., false, cx))
            .child(field("Cache read", &f.cache_read, 110., false, cx));

        let from = h_flex()
            .gap_2()
            .items_end()
            .child(
                v_flex().gap_1().child(label("Effective from", cx)).child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("from-now")
                                .small()
                                .outline()
                                .selected(state.from_now)
                                .label("Now")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(s) = this.form.as_mut() {
                                        s.from_now = true;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("from-date")
                                .small()
                                .outline()
                                .selected(!state.from_now)
                                .label("Date")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(s) = this.form.as_mut() {
                                        s.from_now = false;
                                    }
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .child(
                div()
                    .w(px(170.))
                    .child(Input::new(&f.from).small().disabled(state.from_now)),
            )
            .child(label(
                format!(
                    "YYYY-MM-DD or YYYY-MM-DD HH:MM, in {}",
                    tz.iana_name().unwrap_or("UTC")
                ),
                cx,
            ));

        let long_context = v_flex()
            .gap_2()
            .child(
                Checkbox::new("long-context")
                    .checked(state.long_context)
                    .label("Long-context tier (multipliers above a prompt size)")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        if let Some(s) = this.form.as_mut() {
                            s.long_context = *checked;
                        }
                        cx.notify();
                    })),
            )
            .when(state.long_context, |d| {
                d.child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .child(field(
                            "Above prompt tokens",
                            &f.lc_threshold,
                            130.,
                            false,
                            cx,
                        ))
                        .child(field("Input ×", &f.lc_input, 80., false, cx))
                        .child(field("Output ×", &f.lc_output, 80., false, cx))
                        .child(field("Cache write ×", &f.lc_cache_write, 90., false, cx))
                        .child(field("Cache read ×", &f.lc_cache_read, 90., false, cx)),
                )
            });

        let mut actions = h_flex().gap_2().items_center();
        if editing_user.is_some() {
            actions = actions
                .child(
                    Button::new("save-changed")
                        .small()
                        .primary()
                        .disabled(busy)
                        .label(format!("Price changed from {from_text}"))
                        .tooltip("Close this row at that date and add a new row from then on")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_price(SaveKind::PriceChanged, cx)
                        })),
                )
                .child(
                    Button::new("save-fix")
                        .small()
                        .outline()
                        .disabled(busy)
                        .label("Fix this row")
                        .tooltip("Correct the row in place and re-price everything it covers")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.save_price(SaveKind::FixRow, cx)),
                        ),
                );
        } else {
            actions = actions.child(
                Button::new("save-new")
                    .small()
                    .primary()
                    .disabled(busy)
                    .label("Save user price")
                    .on_click(cx.listener(|this, _, _, cx| this.save_price(SaveKind::New, cx))),
            );
        }
        actions = actions.child(
            Button::new("form-cancel")
                .small()
                .ghost()
                .label("Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.form = None;
                    cx.notify();
                })),
        );

        ui::card(title, cx)
            .child(
                h_flex()
                    .gap_3()
                    .child(field("Provider", &f.provider, 140., false, cx))
                    .child(field(
                        "Model pattern (exact id or glob with *)",
                        &f.pattern,
                        320.,
                        false,
                        cx,
                    )),
            )
            .child(rates)
            .child(from)
            .child(long_context)
            .children(errors.into_iter().map(move |e| {
                div()
                    .text_sm()
                    .text_color(danger)
                    .child(e)
                    .into_any_element()
            }))
            .child(actions)
            .into_any_element()
    }

    fn render_unpriced(&self, data: &PricesData, cx: &mut Context<Self>) -> AnyElement {
        let tz = self.app_state.read(cx).tz();
        let fmt = |ts: jiff::Timestamp| {
            ts.to_zoned_datetime(tz.clone())
                .strftime("%b %-d, %H:%M")
                .to_string()
        };
        let cols = [
            Col::flex("Model"),
            Col::fixed("Provider", 100.0),
            Col::num("Calls", 70.0),
            Col::num("Tokens", 80.0),
            Col::fixed("First seen", 110.0),
            Col::fixed("Last seen", 110.0),
            Col::fixed("", 90.0),
        ];
        let editable = self.editable();
        let rows = data
            .unpriced
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let (provider, model, first) = (u.provider.clone(), u.model.clone(), u.first_seen);
                let tz = tz.clone();
                vec![
                    u.model.clone().into_any_element(),
                    u.provider.clone().into_any_element(),
                    fmt_count(u.events).into_any_element(),
                    fmt_tokens(u.tokens.total()).into_any_element(),
                    fmt(u.first_seen).into_any_element(),
                    fmt(u.last_seen).into_any_element(),
                    Button::new(("add-price", i))
                        .xsmall()
                        .outline()
                        .disabled(!editable)
                        .label("Add price")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let form = PriceForm::for_model(&provider, &model, first, &tz);
                            let target = Some((provider.clone(), model.clone()));
                            this.open_form(form, None, target, window, cx);
                        }))
                        .into_any_element(),
                ]
            })
            .collect();
        ui::card("Unpriced models", cx)
            .child(ui::muted_text(
                "Calls to these models count tokens but no cost until a price is added. \
                 Adding a price re-prices their past calls inside its effective window.",
                cx,
            ))
            .child(if data.unpriced.is_empty() {
                ui::muted_text("None", cx).into_any_element()
            } else {
                ui::table(&cols, rows, cx).into_any_element()
            })
            .into_any_element()
    }

    fn render_price_table(&self, data: &PricesData, cx: &mut Context<Self>) -> AnyElement {
        let tz = self.app_state.read(cx).tz();
        let now = data::now();
        let rows = self.filter.apply(&data.prices, now);
        let total = rows.len();
        let editable = self.editable() && !self.busy;
        let cols = [
            Col::fixed("Provider", 76.0),
            Col::flex("Model pattern"),
            Col::fixed("Source", 66.0),
            Col::fixed("From", 82.0),
            Col::fixed("To", 82.0),
            Col::num("Input", 48.0),
            Col::num("Output", 48.0),
            Col::num("CW 5m", 48.0),
            Col::num("CW 1h", 48.0),
            Col::num("Read", 48.0),
            Col::fixed("Long ctx", 84.0),
            Col::fixed("", 112.0),
        ];
        let muted = cx.theme().muted_foreground;
        let table_rows = rows
            .iter()
            .take(MAX_ROWS)
            .enumerate()
            .map(|(i, p)| {
                let closed = is_closed(p, now);
                vec![
                    p.provider.clone().into_any_element(),
                    div()
                        .when(closed, |d| d.text_color(muted))
                        .child(p.model_pattern.clone())
                        .into_any_element(),
                    source_tag(p.source).into_any_element(),
                    date(Some(p.effective_from), &tz).into_any_element(),
                    date(p.effective_to, &tz).into_any_element(),
                    prices::rate(Some(p.input_per_mtok)).into_any_element(),
                    prices::rate(Some(p.output_per_mtok)).into_any_element(),
                    prices::rate(p.cache_write_5m_per_mtok).into_any_element(),
                    prices::rate(p.cache_write_1h_per_mtok).into_any_element(),
                    prices::rate(p.cache_read_per_mtok).into_any_element(),
                    prices::long_context(p.long_context.as_ref()).into_any_element(),
                    self.row_actions(i, p, editable, now, &tz, cx),
                ]
            })
            .collect();

        let source_button = |id: &'static str, text: &'static str, src: Option<PriceSource>| {
            Button::new(id)
                .xsmall()
                .ghost()
                .selected(self.filter.source == src)
                .label(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter.source = src;
                    cx.notify();
                }))
        };
        let toolbar = h_flex()
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(260.))
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(source_button("src-all", "All", None))
                    .child(source_button("src-user", "User", Some(PriceSource::User)))
                    .child(source_button(
                        "src-imported",
                        "Imported",
                        Some(PriceSource::Imported),
                    ))
                    .child(source_button(
                        "src-bundled",
                        "Bundled",
                        Some(PriceSource::Bundled),
                    )),
            )
            .child(
                Checkbox::new("show-closed")
                    .checked(self.filter.show_closed)
                    .label("Show closed rows")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.filter.show_closed = *checked;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("add-user-price")
                    .small()
                    .primary()
                    .disabled(!editable)
                    .label("Add user price")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_form(PriceForm::default(), None, None, window, cx)
                    })),
            );
        let count = if total > MAX_ROWS {
            format!("Showing {MAX_ROWS} of {total} rows; refine the filter to see the rest.")
        } else {
            format!(
                "{total} rows. Rates are USD per million tokens; user rows win over imported, imported over bundled."
            )
        };
        ui::card("Prices", cx)
            .child(toolbar)
            .child(ui::muted_text(count, cx))
            .child(ui::table(&cols, table_rows, cx))
            .into_any_element()
    }

    fn row_actions(
        &self,
        i: usize,
        p: &ModelPrice,
        editable: bool,
        now: jiff::Timestamp,
        tz: &TimeZone,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(id) = p.id else {
            return div().into_any_element();
        };
        if self.confirm_close == Some(id) {
            return h_flex()
                .gap_1()
                .child(
                    Button::new(("close-yes", i))
                        .xsmall()
                        .danger()
                        .label("Close now")
                        .on_click(cx.listener(move |this, _, _, cx| this.close_row(id, cx))),
                )
                .child(
                    Button::new(("close-no", i))
                        .xsmall()
                        .ghost()
                        .label("Keep")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_close = None;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let is_user = p.source == PriceSource::User;
        let row = p.clone();
        let tz = tz.clone();
        h_flex()
            .gap_1()
            .child(
                Button::new(("edit-price", i))
                    .xsmall()
                    .outline()
                    .disabled(!editable)
                    .label(if is_user { "Edit" } else { "Override" })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let mut form = PriceForm::from_price(&row, &tz);
                        if row.source != PriceSource::User {
                            form.effective_from = super::price_form::EffectiveFrom::At(
                                format_timestamp(row.effective_from, &tz),
                            );
                        }
                        this.open_form(form, Some(row.clone()), None, window, cx);
                    })),
            )
            .when(can_close(p, now), |d| {
                d.child(
                    Button::new(("close-price", i))
                        .xsmall()
                        .ghost()
                        .disabled(!editable)
                        .label("Close")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_close = Some(id);
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_import(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.import_busy;
        let mut card = ui::card("Import prices from LiteLLM", cx)
            .child(ui::muted_text(
                "Downloads model_prices_and_context_window.json from the LiteLLM repository \
                 only when you click. Changed prices close the current imported row now and \
                 open a new one, so past usage keeps its cost. User and bundled rows are never touched.",
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("fetch-litellm")
                            .small()
                            .primary()
                            .loading(busy)
                            .disabled(busy)
                            .label("Fetch latest prices")
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_litellm(cx))),
                    )
                    .child(label("or from a local file:", cx))
                    .child(
                        div()
                            .w(px(420.))
                            .child(Input::new(&self.import_path).small()),
                    )
                    .child(
                        Button::new("load-litellm-file")
                            .small()
                            .outline()
                            .disabled(busy)
                            .label("Load file")
                            .on_click(cx.listener(|this, _, _, cx| this.load_import_file(cx))),
                    ),
            );
        if let Some((_, preview)) = &self.import {
            let cols = [
                Col::fixed("Provider", 100.0),
                Col::flex("Model"),
                Col::fixed("Change", 90.0),
                Col::fixed("Input $/Mtok", 160.0),
                Col::fixed("Output $/Mtok", 160.0),
            ];
            let pair = |old: Option<tbt_core::Decimal>, new: tbt_core::Decimal| match old {
                Some(o) => format!("{} → {}", o.normalize(), new.normalize()),
                None => new.normalize().to_string(),
            };
            let rows = preview
                .lines
                .iter()
                .map(|l| {
                    vec![
                        l.provider.clone().into_any_element(),
                        l.model.clone().into_any_element(),
                        if l.old.is_some() { "changed" } else { "new" }.into_any_element(),
                        pair(l.old.map(|o| o.0), l.new.0).into_any_element(),
                        pair(l.old.map(|o| o.1), l.new.1).into_any_element(),
                    ]
                })
                .collect();
            let can_apply = self.editable() && !self.busy && !preview.is_empty();
            card = card
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(format!("Preview: {}", preview.summary())),
                )
                .when(!preview.lines.is_empty(), |d| {
                    d.child(ui::table(&cols, rows, cx))
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("apply-import")
                                .small()
                                .primary()
                                .disabled(!can_apply)
                                .label(format!("Apply {} changes", preview.changed + preview.new))
                                .on_click(cx.listener(|this, _, _, cx| this.apply_import(cx))),
                        )
                        .child(
                            Button::new("discard-import")
                                .small()
                                .ghost()
                                .label("Discard")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.import = None;
                                    cx.notify();
                                })),
                        ),
                );
        }
        card.into_any_element()
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = &self.general;
        let path = self
            .config
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".to_owned());
        let effective = super::general::effective_data_dir(self.data.db_path().as_deref())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none (mock data)".to_owned());
        let danger = cx.theme().danger;
        let load_error = self.config.loaded.as_ref().err().cloned();
        let privacy = v_flex()
            .gap_2()
            .child(label("Message privacy", cx))
            .children(PRIVACY_OPTIONS.into_iter().map(|(level, name, help)| {
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new(SharedString::from(format!("privacy-{name}")))
                            .small()
                            .outline()
                            .w(px(150.))
                            .selected(g.privacy == level)
                            .label(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.general.privacy = level;
                                cx.notify();
                            })),
                    )
                    .child(ui::muted_text(help, cx))
            }));
        let retention = h_flex()
            .gap_3()
            .items_center()
            .child(
                Checkbox::new("retention-on")
                    .checked(g.retention_on)
                    .label("Drop raw_json of events older than")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.general.retention_on = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(px(80.))
                    .child(Input::new(&g.retention).small().disabled(!g.retention_on)),
            )
            .child(label("days (off keeps raw JSON forever)", cx));
        ui::card("General", cx)
            .child(ui::muted_text(format!("Config file: {path}"), cx))
            .children(load_error.map(|e| {
                div()
                    .text_sm()
                    .text_color(danger)
                    .child(format!("Could not read the config file: {e}"))
            }))
            .child(privacy)
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(field("Sync interval (minutes)", &g.interval, 160., false, cx))
                    .child(label(
                        format!("{SYNC_MIN_MINUTES} to {SYNC_MAX_MINUTES} minutes"),
                        cx,
                    )),
            )
            .child(retention)
            .child(
                v_flex()
                    .gap_1()
                    .child(label("Data directory override", cx))
                    .child(div().w(px(520.)).child(Input::new(&g.data_dir).small()))
                    .child(ui::muted_text(
                        format!(
                            "In use now: {effective}. Leave empty for the default; a change needs a restart."
                        ),
                        cx,
                    )),
            )
            .when(crate::background::available(), |d| {
                d.child(
                    v_flex()
                        .gap_1()
                        .child(label("Collect while closed", cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("autostart-on")
                                        .small()
                                        .outline()
                                        .label("Turn on")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_autostart(true, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("autostart-off")
                                        .small()
                                        .outline()
                                        .label("Turn off")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_autostart(false, window, cx)
                                        })),
                                ),
                        )
                        .child(ui::muted_text(
                            "Starts tbt-daemon now and at every login through the desktop's background permission, so syncs continue while the app is closed.",
                            cx,
                        )),
                )
            })
            .children(g.errors.iter().map(|e| {
                div()
                    .text_sm()
                    .text_color(danger)
                    .child(e.clone())
            }))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new("save-general")
                            .small()
                            .primary()
                            .disabled(self.config.path.is_none())
                            .label("Save settings")
                            .on_click(cx.listener(|this, _, _, cx| this.save_general(cx))),
                    )
                    .child(label("Changes take effect on the next sync.", cx)),
            )
            .into_any_element()
    }

    fn render_generic(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (danger, warning) = (t.danger, t.warning);
        let mut report = v_flex().gap_1();
        match &self.generic_check {
            None => {}
            Some(Err(e)) => {
                report = report.child(div().text_sm().text_color(danger).child(e.clone()));
            }
            Some(Ok(checked)) if checked.entries.is_empty() => {
                report = report.child(ui::muted_text("No [[generic_jsonl]] entries.", cx));
            }
            Some(Ok(checked)) => {
                for e in &checked.entries {
                    let ok = e.errors.is_empty();
                    report = report.child(div().text_sm().font_semibold().child(format!(
                        "{}: {}",
                        e.label,
                        if ok { "valid" } else { "invalid" }
                    )));
                    for msg in &e.errors {
                        report = report.child(
                            div()
                                .pl_3()
                                .text_sm()
                                .text_color(danger)
                                .child(format!("error: {msg}")),
                        );
                    }
                    for msg in &e.warnings {
                        report = report.child(
                            div()
                                .pl_3()
                                .text_sm()
                                .text_color(warning)
                                .child(format!("warning: {msg}")),
                        );
                    }
                }
            }
        }
        let empty = self.generic_editor.read(cx).value().trim().is_empty();
        ui::card("Generic JSONL adapters", cx)
            .child(ui::muted_text(
                "Each [[generic_jsonl]] table maps fields of your own JSONL logs to usage events. \
                 JSON paths start with `$`; constants are plain text. Saved to the config file and \
                 used from the next sync.",
                cx,
            ))
            .child(Textarea::new(&self.generic_editor).h(px(340.)))
            .child(report)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("generic-validate")
                            .small()
                            .outline()
                            .label("Validate")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.validate_generic(cx);
                            })),
                    )
                    .child(
                        Button::new("generic-save")
                            .small()
                            .primary()
                            .disabled(self.config.path.is_none())
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_generic(cx))),
                    )
                    .when(empty, |d| {
                        d.child(
                            Button::new("generic-example")
                                .small()
                                .ghost()
                                .label("Insert example")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let text = super::generic_jsonl::EXAMPLE;
                                    this.generic_editor
                                        .update(cx, |s, cx| s.set_value(text, window, cx));
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_export(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.app_state.read(cx);
        let harnesses = if state.is_all() {
            "all harnesses".to_owned()
        } else {
            state
                .selected
                .iter()
                .map(|h| h.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let scope = format!(
            "Uses the date range and harness filter of the top bar: {}, {harnesses}.",
            state.range.label()
        );
        let datasets = h_flex().gap_1().children(Dataset::ALL.into_iter().map(|d| {
            Button::new(SharedString::from(format!("export-{}", d.label())))
                .small()
                .outline()
                .selected(self.export_dataset == d)
                .label(d.label())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.set_export_dataset(d, window, cx)),
                )
        }));
        let help = match self.export_dataset {
            Dataset::Sessions => "One row per session: tokens, cost (USD), models, project.",
            Dataset::HourlyUsage => {
                "One row per UTC hour and harness, plus one per hour and model (column `group`)."
            }
        };
        ui::card("Export", cx)
            .child(ui::muted_text(scope, cx))
            .child(datasets)
            .child(ui::muted_text(help, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(520.)).child(Input::new(&self.export_path).small()))
                    .child(
                        Button::new("export-browse")
                            .small()
                            .ghost()
                            .label("Browse…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.browse_export(window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("export-csv")
                            .small()
                            .primary()
                            .label("Export CSV")
                            .on_click(cx.listener(|this, _, _, cx| this.export(Format::Csv, cx))),
                    )
                    .child(
                        Button::new("export-json")
                            .small()
                            .outline()
                            .label("Export JSON")
                            .on_click(cx.listener(|this, _, _, cx| this.export(Format::Json, cx))),
                    )
                    .child(label(
                        "The extension is set from the format. Money is decimal USD, times are UTC (RFC 3339).",
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_about(&self, cx: &mut Context<Self>) -> AnyElement {
        let db = self.data.db_path();
        let rows = about::rows(db.as_deref(), self.config.path.as_deref());
        let link = |id: &'static str, text: &'static str, url: &'static str| {
            Button::new(id)
                .debug_selector(|| id.into())
                .small()
                .outline()
                .label(text)
                .tooltip(url)
                .on_click(move |_, _, cx| cx.open_url(url))
        };
        ui::card(about::APP_NAME, cx)
            .child(ui::muted_text(
                "Tracks the token usage and cost of AI coding agents from their local logs. \
                 Everything stays on this machine.",
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .children(rows.into_iter().map(|(name, text)| {
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .child(div().w(px(110.)).child(label(name, cx)))
                            .child(text)
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(link("about-source", "Source code", about::REPO_URL))
                    .child(link("about-issues", "Report an issue", about::ISSUES_URL))
                    .child(link("about-license", "License text", about::LICENSE_URL)),
            )
            .child(ui::muted_text(
                "This program is free software under the GNU General Public License, version 3 \
                 or later. It comes with ABSOLUTELY NO WARRANTY.",
                cx,
            ))
            .into_any_element()
    }
}
