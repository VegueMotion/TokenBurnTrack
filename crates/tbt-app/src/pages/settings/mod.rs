pub mod general;
pub mod generic_jsonl;
pub mod ops;
pub mod price_form;
pub mod prices;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, AppContext, Context, Entity, IntoElement, Render, Subscription, Task, Window};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use tbt_collector::{CollectorConfig, default_config_path};
use tbt_core::{ModelPrice, PriceId, PrivacyLevel};
use tbt_store::{Store, UnpricedModel};

use self::general::GeneralForm;
use self::generic_jsonl::Checked;
use self::ops::ImportPreview;
use self::price_form::{EffectiveFrom, LongContextForm, PriceForm, SaveKind, plan_save};
use self::prices::PriceFilter;
use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::data::{self, DataSource};
use crate::export::{self, Dataset, Format};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Prices,
    Import,
    General,
    Adapters,
    Export,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Prices,
        Tab::Import,
        Tab::General,
        Tab::Adapters,
        Tab::Export,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Prices => "Prices",
            Tab::Import => "LiteLLM import",
            Tab::General => "General",
            Tab::Adapters => "Generic JSONL",
            Tab::Export => "Export",
        }
    }
}

enum Notice {
    Info(String),
    Error(String),
}

struct PricesData {
    prices: Vec<ModelPrice>,
    unpriced: Vec<UnpricedModel>,
}

struct FormInputs {
    provider: Entity<InputState>,
    pattern: Entity<InputState>,
    input: Entity<InputState>,
    output: Entity<InputState>,
    cache_write_5m: Entity<InputState>,
    cache_write_1h: Entity<InputState>,
    cache_read: Entity<InputState>,
    from: Entity<InputState>,
    lc_threshold: Entity<InputState>,
    lc_input: Entity<InputState>,
    lc_output: Entity<InputState>,
    lc_cache_write: Entity<InputState>,
    lc_cache_read: Entity<InputState>,
}

struct FormState {
    editing: Option<ModelPrice>,
    unpriced: Option<(String, String)>,
    from_now: bool,
    long_context: bool,
    errors: Vec<String>,
}

struct ConfigState {
    path: Option<PathBuf>,
    loaded: Result<CollectorConfig, String>,
}

struct GeneralInputs {
    interval: Entity<InputState>,
    retention: Entity<InputState>,
    data_dir: Entity<InputState>,
    privacy: PrivacyLevel,
    retention_on: bool,
    errors: Vec<String>,
}

pub struct SettingsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    tab: Tab,
    notice: Option<Notice>,
    busy: bool,
    prices: Loadable<PricesData>,
    filter: PriceFilter,
    search: Entity<InputState>,
    form_inputs: FormInputs,
    form: Option<FormState>,
    confirm_close: Option<PriceId>,
    import_path: Entity<InputState>,
    import: Option<(Arc<String>, ImportPreview)>,
    import_busy: bool,
    config: ConfigState,
    general: GeneralInputs,
    generic_editor: Entity<TextareaState>,
    generic_check: Option<Result<Checked, String>>,
    export_path: Entity<InputState>,
    export_dataset: Dataset,
    _load: Task<()>,
    _write: Task<()>,
    _side: Task<()>,
    _subscriptions: Vec<Subscription>,
}

fn input(window: &mut Window, cx: &mut App, placeholder: &str) -> Entity<InputState> {
    let placeholder = placeholder.to_owned();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
}

fn value(e: &Entity<InputState>, cx: &App) -> String {
    e.read(cx).value().to_string()
}

fn set(e: &Entity<InputState>, text: &str, window: &mut Window, cx: &mut App) {
    let text = text.to_owned();
    e.update(cx, |s, cx| s.set_value(text, window, cx));
}

fn load_config() -> ConfigState {
    let path = default_config_path();
    let loaded = match &path {
        Some(p) => CollectorConfig::load(p).map_err(|e| e.to_string()),
        None => Err("no config directory is known for this platform".to_owned()),
    };
    ConfigState { path, loaded }
}

impl SettingsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = input(window, cx, "Filter by provider or model…");
        let form_inputs = FormInputs {
            provider: input(window, cx, "anthropic"),
            pattern: input(window, cx, "claude-sonnet-5-5 or claude-sonnet-5*"),
            input: input(window, cx, "3"),
            output: input(window, cx, "15"),
            cache_write_5m: input(window, cx, "optional"),
            cache_write_1h: input(window, cx, "optional"),
            cache_read: input(window, cx, "optional"),
            from: input(window, cx, "YYYY-MM-DD"),
            lc_threshold: input(window, cx, "200000"),
            lc_input: input(window, cx, "2"),
            lc_output: input(window, cx, "1.5"),
            lc_cache_write: input(window, cx, "2"),
            lc_cache_read: input(window, cx, "2"),
        };
        let import_path = input(window, cx, "/path/to/model_prices_and_context_window.json");

        let config = load_config();
        let form = GeneralForm::from_config(config.loaded.as_ref().unwrap_or(&Default::default()));
        let general = GeneralInputs {
            interval: input(window, cx, "60"),
            retention: input(window, cx, "30"),
            data_dir: input(window, cx, "default"),
            privacy: form.privacy,
            retention_on: form.retention_enabled,
            errors: Vec::new(),
        };
        set(&general.interval, &form.sync_interval, window, cx);
        set(&general.retention, &form.retention_days, window, cx);
        set(&general.data_dir, &form.data_dir, window, cx);

        let generic_editor = cx.new(|cx| TextareaState::new(window, cx).rows(18));
        if let Ok(cfg) = &config.loaded {
            let text = generic_jsonl::entries_text(cfg);
            generic_editor.update(cx, |s, cx| s.set_value(text, window, cx));
        }

        let export_path = input(window, cx, "~/Downloads/tokenburntrack.csv");
        let default_export = default_export_path(Dataset::Sessions);
        set(&export_path, &default_export, window, cx);

        let subscriptions = vec![
            cx.subscribe(&app_state, |this, _, ev: &AppStateEvent, cx| {
                if *ev == AppStateEvent::DataChanged {
                    this.reload(cx);
                }
            }),
            cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.filter.search = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];
        let mut page = SettingsPage {
            app_state,
            data,
            tab: Tab::Prices,
            notice: None,
            busy: false,
            prices: Loadable::Loading,
            filter: PriceFilter::default(),
            search,
            form_inputs,
            form: None,
            confirm_close: None,
            import_path,
            import: None,
            import_busy: false,
            config,
            general,
            generic_editor,
            generic_check: None,
            export_path,
            export_dataset: Dataset::Sessions,
            _load: Task::ready(()),
            _write: Task::ready(()),
            _side: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.reload(cx);
        page
    }

    fn editable(&self) -> bool {
        self.data.db_path().is_some()
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || {
                Ok(PricesData {
                    prices: ops::load_prices(data.db_path().as_deref())?,
                    unpriced: data.unpriced_models()?,
                })
            },
            |this, state, _| this.prices = state,
        );
    }

    fn info(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(Notice::Info(text.into()));
        cx.notify();
    }

    fn error(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(Notice::Error(text.into()));
        cx.notify();
    }

    fn run_write<T: Send + 'static>(
        &mut self,
        label: &str,
        job: impl FnOnce(&mut Store) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.data.db_path() else {
            self.error("Editing is disabled with mock data.", cx);
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.info(format!("{label}…"), cx);
        self._write = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::with_writable_store(&db, job) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(v) => {
                        done(this, v, cx);
                        this.app_state
                            .update(cx, |_, cx| cx.emit(AppStateEvent::DataChanged));
                    }
                    Err(e) => this.notice = Some(Notice::Error(e.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn run_side<T: Send + 'static>(
        &mut self,
        job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self._side = cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            this.update(cx, |this, cx| {
                done(this, result, cx);
                cx.notify();
            })
            .ok();
        });
    }

    fn open_form(
        &mut self,
        form: PriceForm,
        editing: Option<ModelPrice>,
        unpriced: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let f = &self.form_inputs;
        set(&f.provider, &form.provider, window, cx);
        set(&f.pattern, &form.model_pattern, window, cx);
        set(&f.input, &form.input, window, cx);
        set(&f.output, &form.output, window, cx);
        set(&f.cache_write_5m, &form.cache_write_5m, window, cx);
        set(&f.cache_write_1h, &form.cache_write_1h, window, cx);
        set(&f.cache_read, &form.cache_read, window, cx);
        let from_now = match &form.effective_from {
            EffectiveFrom::Now => {
                set(&f.from, "", window, cx);
                true
            }
            EffectiveFrom::At(text) => {
                set(&f.from, text, window, cx);
                false
            }
        };
        let lc = form.long_context.clone().unwrap_or_default();
        set(&f.lc_threshold, &lc.threshold, window, cx);
        set(&f.lc_input, &lc.input, window, cx);
        set(&f.lc_output, &lc.output, window, cx);
        set(&f.lc_cache_write, &lc.cache_write, window, cx);
        set(&f.lc_cache_read, &lc.cache_read, window, cx);
        self.form = Some(FormState {
            editing,
            unpriced,
            from_now,
            long_context: form.long_context.is_some(),
            errors: Vec::new(),
        });
        self.tab = Tab::Prices;
        cx.notify();
    }

    fn read_form(&self, cx: &App) -> PriceForm {
        let f = &self.form_inputs;
        let state = self.form.as_ref();
        PriceForm {
            provider: value(&f.provider, cx),
            model_pattern: value(&f.pattern, cx),
            input: value(&f.input, cx),
            output: value(&f.output, cx),
            cache_write_5m: value(&f.cache_write_5m, cx),
            cache_write_1h: value(&f.cache_write_1h, cx),
            cache_read: value(&f.cache_read, cx),
            effective_from: if state.is_some_and(|s| s.from_now) {
                EffectiveFrom::Now
            } else {
                EffectiveFrom::At(value(&f.from, cx))
            },
            long_context: state.filter(|s| s.long_context).map(|_| LongContextForm {
                threshold: value(&f.lc_threshold, cx),
                input: value(&f.lc_input, cx),
                output: value(&f.lc_output, cx),
                cache_write: value(&f.lc_cache_write, cx),
                cache_read: value(&f.lc_cache_read, cx),
            }),
        }
    }

    fn save_price(&mut self, kind: SaveKind, cx: &mut Context<Self>) {
        let form = self.read_form(cx);
        let tz = self.app_state.read(cx).tz();
        let Some(state) = self.form.as_mut() else {
            return;
        };
        let price = match form.build(data::now(), &tz) {
            Ok(p) => p,
            Err(errors) => {
                state.errors = errors.iter().map(ToString::to_string).collect();
                cx.notify();
                return;
            }
        };
        let plan = match plan_save(kind, state.editing.as_ref(), price) {
            Ok(p) => p,
            Err(e) => {
                state.errors = vec![e];
                cx.notify();
                return;
            }
        };
        state.errors.clear();
        let unpriced = state.unpriced.clone();
        let label = plan.describe(&tz);
        self.run_write(
            "Saving price",
            move |store| {
                ops::apply_plan(
                    store,
                    &plan,
                    unpriced.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                )
            },
            move |this, out, cx| {
                this.form = None;
                this.info(format!("{label}: {}.", out.summary()), cx);
            },
            cx,
        );
    }

    fn close_row(&mut self, id: PriceId, cx: &mut Context<Self>) {
        self.confirm_close = None;
        let now = data::now();
        self.run_write(
            "Closing price row",
            move |store| ops::close_row(store, id, now),
            |this, out, cx| this.info(format!("Row closed: {}.", out.summary()), cx),
            cx,
        );
    }

    fn preview_from(
        &mut self,
        source: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.import_busy {
            return;
        }
        self.import_busy = true;
        self.import = None;
        self.info("Reading price list…", cx);
        let db = self.data.db_path();
        let now = data::now();
        self.run_side(
            move || {
                let json = source()?;
                let preview = ops::preview_import(ops::load_prices(db.as_deref())?, &json, now)?;
                Ok((Arc::new(json), preview))
            },
            |this, result, cx| {
                this.import_busy = false;
                match result {
                    Ok((json, preview)) => {
                        this.info(format!("Preview: {}.", preview.summary()), cx);
                        this.import = Some((json, preview));
                    }
                    Err(e) => this.error(format!("Import failed: {e:#}"), cx),
                }
            },
            cx,
        );
    }

    fn fetch_litellm(&mut self, cx: &mut Context<Self>) {
        self.preview_from(ops::fetch_litellm, cx);
    }

    fn load_import_file(&mut self, cx: &mut Context<Self>) {
        let text = value(&self.import_path, cx);
        let path = export::expand_home(text.trim(), export::home_dir().as_deref());
        if text.trim().is_empty() {
            self.error("Enter the path of a LiteLLM price JSON file.", cx);
            return;
        }
        self.preview_from(move || ops::read_price_file(&path), cx);
    }

    fn apply_import(&mut self, cx: &mut Context<Self>) {
        let Some((json, _)) = self.import.clone() else {
            return;
        };
        let now = data::now();
        self.run_write(
            "Applying import",
            move |store| ops::apply_import(store, &json, now),
            |this, out, cx| {
                this.import = None;
                this.info(
                    format!(
                        "Imported: {} rows closed, {} rows added; {}.",
                        out.closed,
                        out.opened,
                        out.recompute.summary()
                    ),
                    cx,
                );
            },
            cx,
        );
    }

    fn save_general(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.config.path.clone() else {
            self.error("No config file location is known for this platform.", cx);
            return;
        };
        let g = &self.general;
        let form = GeneralForm {
            privacy: g.privacy,
            sync_interval: value(&g.interval, cx),
            data_dir: value(&g.data_dir, cx),
            retention_enabled: g.retention_on,
            retention_days: value(&g.retention, cx),
        };
        let mut cfg = match CollectorConfig::load(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                self.error(format!("Not saved: {e}"), cx);
                return;
            }
        };
        let before = cfg.clone();
        if let Err(errors) = form.apply(&mut cfg) {
            self.general.errors = errors;
            cx.notify();
            return;
        }
        self.general.errors.clear();
        match cfg.save(&path) {
            Ok(()) => {
                let restart = if general::data_dir_changed(&before, &cfg) {
                    " Restart the app to use the new data directory."
                } else {
                    ""
                };
                self.config.loaded = Ok(cfg);
                self.info(
                    format!(
                        "Saved to {}. Changes take effect on the next sync.{restart}",
                        path.display()
                    ),
                    cx,
                );
            }
            Err(e) => self.error(format!("Not saved: {e}"), cx),
        }
    }

    fn validate_generic(&mut self, cx: &mut Context<Self>) -> Option<Checked> {
        let text = self.generic_editor.read(cx).value().to_string();
        let result = generic_jsonl::check(&text);
        self.generic_check = Some(result.clone());
        cx.notify();
        result.ok().filter(Checked::is_valid)
    }

    fn save_generic(&mut self, cx: &mut Context<Self>) {
        let Some(checked) = self.validate_generic(cx) else {
            self.error("Not saved: fix the errors shown below the editor.", cx);
            return;
        };
        let Some(path) = self.config.path.clone() else {
            self.error("No config file location is known for this platform.", cx);
            return;
        };
        let result = CollectorConfig::load(&path).and_then(|mut cfg| {
            cfg.generic_jsonl = checked.config.generic_jsonl.clone();
            cfg.save(&path).map(|()| cfg)
        });
        match result {
            Ok(cfg) => {
                let n = cfg.generic_jsonl.len();
                self.config.loaded = Ok(cfg);
                self.info(
                    format!(
                        "Saved {n} generic JSONL adapter(s) to {}. They are picked up on the next sync.",
                        path.display()
                    ),
                    cx,
                );
            }
            Err(e) => self.error(format!("Not saved: {e}"), cx),
        }
    }

    fn set_export_dataset(
        &mut self,
        dataset: Dataset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = value(&self.export_path, cx);
        if current == default_export_path(self.export_dataset) {
            set(&self.export_path, &default_export_path(dataset), window, cx);
        }
        self.export_dataset = dataset;
        cx.notify();
    }

    fn browse_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = export::home_dir();
        let dir = export::default_dir(home.as_deref());
        let name = export::file_name(self.export_dataset, Format::Csv, today());
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        self._side = cx.spawn_in(window, async move |this, cx| {
            let picked = rx.await;
            this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(path))) => {
                    set(&this.export_path, &path.display().to_string(), window, cx);
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => this.error(
                    format!("The file dialog is not available ({e:#}); type a path instead."),
                    cx,
                ),
                Err(_) => this.error("The file dialog was closed unexpectedly.", cx),
            })
            .ok();
        });
    }

    fn set_autostart(&mut self, enable: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.info("Waiting for the system to answer…", cx);
        self._side = cx.spawn_in(window, async move |this, cx| {
            let result = crate::background::request_autostart(enable).await;
            this.update(cx, |this, cx| match result {
                Ok(true) => this.info(
                    "Background collection is on: tbt-daemon starts at login and syncs on the configured interval.",
                    cx,
                ),
                Ok(false) if enable => this.error(
                    "Background collection was not allowed. You can change this in your desktop's app permissions.",
                    cx,
                ),
                Ok(false) => this.info(
                    "Background collection is off from the next login. A tbt-daemon that is running now keeps running until you log out.",
                    cx,
                ),
                Err(e) => this.error(
                    format!("The background portal is not available: {e:#}"),
                    cx,
                ),
            })
            .ok();
        });
    }

    fn export(&mut self, format: Format, cx: &mut Context<Self>) {
        let text = value(&self.export_path, cx);
        let Some(path) = export::target_path(&text, format, export::home_dir().as_deref()) else {
            self.error("Enter a file path to export to.", cx);
            return;
        };
        let filter = self.app_state.read(cx).query_filter(data::now());
        let (data, dataset) = (self.data.clone(), self.export_dataset);
        self.info(format!("Exporting {}…", dataset.label()), cx);
        self.run_side(
            move || {
                let out = export::build(data.as_ref(), &filter, dataset, format)?;
                export::write(&path, &out)?;
                Ok((out.rows, path))
            },
            move |this, result, cx| match result {
                Ok((rows, path)) => this.info(
                    format!(
                        "Wrote {rows} {} rows to {} ({}).",
                        dataset.label().to_lowercase(),
                        path.display(),
                        format.label()
                    ),
                    cx,
                ),
                Err(e) => this.error(format!("Export failed: {e:#}"), cx),
            },
            cx,
        );
    }
}

fn today() -> jiff::civil::Date {
    data::now()
        .to_zoned_datetime(jiff::tz::TimeZone::system())
        .date()
}

fn default_export_path(dataset: Dataset) -> String {
    let home = export::home_dir();
    let name = export::file_name(dataset, Format::Csv, today());
    let path = export::default_dir(home.as_deref()).join(name);
    match (
        &home,
        path.strip_prefix(home.as_deref().unwrap_or("/nonexistent".as_ref())),
    ) {
        (Some(_), Ok(rest)) => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::page_scroll("settings-scroll", self.render_page(cx))
    }
}
