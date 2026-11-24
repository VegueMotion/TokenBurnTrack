use std::fmt;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use tbt_core::{Decimal, LongContextTier, ModelPrice, PriceId, PriceSource};
use tbt_pricing::{PriceCatalog, PricingError};

const MAX_NUMBER_CHARS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberError {
    Empty,
    Negative,
    Invalid,
    TooPrecise,
}

impl fmt::Display for NumberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NumberError::Empty => "required",
            NumberError::Negative => "must not be negative",
            NumberError::Invalid => "must be a plain decimal number such as 3 or 0.075",
            NumberError::TooPrecise => "has too many digits",
        })
    }
}

pub fn parse_decimal(text: &str) -> Result<Decimal, NumberError> {
    let s = text.trim();
    if s.is_empty() {
        return Err(NumberError::Empty);
    }
    if s.starts_with('-') {
        return Err(NumberError::Negative);
    }
    if s.len() > MAX_NUMBER_CHARS {
        return Err(NumberError::TooPrecise);
    }
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) {
        return Err(NumberError::Invalid);
    }
    let int = if int.is_empty() { "0" } else { int };
    let normalized = if frac.is_empty() {
        int.to_owned()
    } else {
        format!("{int}.{frac}")
    };
    Decimal::from_str_exact(&normalized).map_err(|_| NumberError::TooPrecise)
}

fn parse_optional(text: &str) -> Result<Option<Decimal>, NumberError> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        parse_decimal(text).map(Some)
    }
}

pub fn parse_tokens(text: &str) -> Result<u64, NumberError> {
    let s = text.trim();
    if s.is_empty() {
        return Err(NumberError::Empty);
    }
    if s.starts_with('-') {
        return Err(NumberError::Negative);
    }
    let digits: String = s.chars().filter(|c| *c != '_' && *c != ',').collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(NumberError::Invalid);
    }
    digits.parse().map_err(|_| NumberError::TooPrecise)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveFrom {
    Now,
    At(String),
}

pub fn parse_timestamp(text: &str, tz: &TimeZone) -> Result<Timestamp, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("enter a date such as 2026-11-01, or pick Now".to_owned());
    }
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(ts);
    }
    let civil = s
        .parse::<DateTime>()
        .or_else(|_| {
            s.parse::<Date>()
                .map(|d| d.to_datetime(jiff::civil::Time::midnight()))
        })
        .map_err(|_| format!("`{s}` is not a date (YYYY-MM-DD or YYYY-MM-DD HH:MM)"))?;
    civil
        .to_zoned_datetime(tz.clone())
        .map(|z| z.timestamp())
        .map_err(|e| e.to_string())
}

pub fn format_timestamp(ts: Timestamp, tz: &TimeZone) -> String {
    let z = ts.to_zoned_datetime(tz.clone());
    if z.start_of_day().is_ok_and(|s| s.timestamp() == ts) {
        z.strftime("%Y-%m-%d").to_string()
    } else {
        z.strftime("%Y-%m-%d %H:%M").to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongContextForm {
    pub threshold: String,
    pub input: String,
    pub output: String,
    pub cache_write: String,
    pub cache_read: String,
}

impl Default for LongContextForm {
    fn default() -> Self {
        LongContextForm {
            threshold: "200000".into(),
            input: "2".into(),
            output: "1.5".into(),
            cache_write: "2".into(),
            cache_read: "2".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceForm {
    pub provider: String,
    pub model_pattern: String,
    pub input: String,
    pub output: String,
    pub cache_write_5m: String,
    pub cache_write_1h: String,
    pub cache_read: String,
    pub effective_from: EffectiveFrom,
    pub long_context: Option<LongContextForm>,
}

impl Default for PriceForm {
    fn default() -> Self {
        PriceForm {
            provider: String::new(),
            model_pattern: String::new(),
            input: String::new(),
            output: String::new(),
            cache_write_5m: String::new(),
            cache_write_1h: String::new(),
            cache_read: String::new(),
            effective_from: EffectiveFrom::Now,
            long_context: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Provider,
    Pattern,
    Input,
    Output,
    CacheWrite5m,
    CacheWrite1h,
    CacheRead,
    EffectiveFrom,
    LcThreshold,
    LcInput,
    LcOutput,
    LcCacheWrite,
    LcCacheRead,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Provider => "Provider",
            Field::Pattern => "Model pattern",
            Field::Input => "Input $/Mtok",
            Field::Output => "Output $/Mtok",
            Field::CacheWrite5m => "Cache write 5m $/Mtok",
            Field::CacheWrite1h => "Cache write 1h $/Mtok",
            Field::CacheRead => "Cache read $/Mtok",
            Field::EffectiveFrom => "Effective from",
            Field::LcThreshold => "Long-context threshold",
            Field::LcInput => "Long-context input ×",
            Field::LcOutput => "Long-context output ×",
            Field::LcCacheWrite => "Long-context cache write ×",
            Field::LcCacheRead => "Long-context cache read ×",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field.label(), self.message)
    }
}

fn rate_text(d: Option<Decimal>) -> String {
    d.map(|d| d.to_string()).unwrap_or_default()
}

impl PriceForm {
    pub fn from_price(p: &ModelPrice, tz: &TimeZone) -> PriceForm {
        PriceForm {
            provider: p.provider.clone(),
            model_pattern: p.model_pattern.clone(),
            input: p.input_per_mtok.to_string(),
            output: p.output_per_mtok.to_string(),
            cache_write_5m: rate_text(p.cache_write_5m_per_mtok),
            cache_write_1h: rate_text(p.cache_write_1h_per_mtok),
            cache_read: rate_text(p.cache_read_per_mtok),
            effective_from: EffectiveFrom::At(format_timestamp(p.effective_from, tz)),
            long_context: p.long_context.as_ref().map(|lc| LongContextForm {
                threshold: lc.threshold_tokens.to_string(),
                input: lc.input.to_string(),
                output: lc.output.to_string(),
                cache_write: lc.cache_write.to_string(),
                cache_read: lc.cache_read.to_string(),
            }),
        }
    }

    pub fn for_model(provider: &str, model: &str, first_seen: Timestamp, tz: &TimeZone) -> Self {
        let day = first_seen
            .to_zoned_datetime(tz.clone())
            .start_of_day()
            .map(|z| z.timestamp())
            .unwrap_or(first_seen);
        PriceForm {
            provider: provider.to_owned(),
            model_pattern: model.to_owned(),
            effective_from: EffectiveFrom::At(format_timestamp(day, tz)),
            ..PriceForm::default()
        }
    }

    pub fn build(&self, now: Timestamp, tz: &TimeZone) -> Result<ModelPrice, Vec<FieldError>> {
        let mut errors = Vec::new();
        let mut err = |field: Field, message: String| errors.push(FieldError { field, message });

        let provider = self.provider.trim();
        if provider.is_empty() {
            err(Field::Provider, "required".into());
        } else if provider.contains(char::is_whitespace) {
            err(Field::Provider, "must not contain spaces".into());
        }
        let pattern = self.model_pattern.trim();
        if pattern.is_empty() {
            err(Field::Pattern, "required".into());
        } else if pattern.contains(char::is_whitespace) {
            err(Field::Pattern, "must not contain spaces".into());
        }

        let mut required = |field: Field, text: &str| match parse_decimal(text) {
            Ok(d) => Some(d),
            Err(e) => {
                err(field, e.to_string());
                None
            }
        };
        let input = required(Field::Input, &self.input);
        let output = required(Field::Output, &self.output);
        let mut optional = |field: Field, text: &str| match parse_optional(text) {
            Ok(d) => d,
            Err(e) => {
                err(field, e.to_string());
                None
            }
        };
        let cache_write_5m = optional(Field::CacheWrite5m, &self.cache_write_5m);
        let cache_write_1h = optional(Field::CacheWrite1h, &self.cache_write_1h);
        let cache_read = optional(Field::CacheRead, &self.cache_read);

        let effective_from = match &self.effective_from {
            EffectiveFrom::Now => Some(now),
            EffectiveFrom::At(text) => match parse_timestamp(text, tz) {
                Ok(ts) => Some(ts),
                Err(e) => {
                    err(Field::EffectiveFrom, e);
                    None
                }
            },
        };

        let long_context = self.long_context.as_ref().and_then(|lc| {
            let threshold = match parse_tokens(&lc.threshold) {
                Ok(0) => {
                    err(Field::LcThreshold, "must be above 0".into());
                    None
                }
                Ok(n) => Some(n),
                Err(e) => {
                    err(Field::LcThreshold, e.to_string());
                    None
                }
            };
            let mut multiplier = |field: Field, text: &str| match parse_decimal(text) {
                Ok(d) => Some(d),
                Err(e) => {
                    err(field, e.to_string());
                    None
                }
            };
            let input = multiplier(Field::LcInput, &lc.input);
            let output = multiplier(Field::LcOutput, &lc.output);
            let cache_write = multiplier(Field::LcCacheWrite, &lc.cache_write);
            let cache_read = multiplier(Field::LcCacheRead, &lc.cache_read);
            Some(LongContextTier {
                threshold_tokens: threshold?,
                input: input?,
                output: output?,
                cache_write: cache_write?,
                cache_read: cache_read?,
            })
        });

        if !errors.is_empty() {
            return Err(errors);
        }
        let (Some(input), Some(output), Some(effective_from)) = (input, output, effective_from)
        else {
            return Err(errors);
        };
        let price = ModelPrice {
            id: None,
            provider: provider.to_owned(),
            model_pattern: pattern.to_owned(),
            effective_from,
            effective_to: None,
            input_per_mtok: input,
            output_per_mtok: output,
            cache_write_5m_per_mtok: cache_write_5m,
            cache_write_1h_per_mtok: cache_write_1h,
            cache_read_per_mtok: cache_read,
            long_context,
            source: PriceSource::User,
        };
        check_row(&price).map_err(|e| vec![e])?;
        Ok(price)
    }
}

fn check_row(price: &ModelPrice) -> Result<(), FieldError> {
    match PriceCatalog::load(vec![price.clone()]) {
        Ok(_) => Ok(()),
        Err(PricingError::Pattern { message, .. }) => Err(FieldError {
            field: Field::Pattern,
            message: format!("invalid glob: {message}"),
        }),
        Err(e) => Err(FieldError {
            field: Field::EffectiveFrom,
            message: e.to_string(),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    New,
    PriceChanged,
    FixRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricePlan {
    Insert(ModelPrice),
    Supersede {
        close: PriceId,
        effective_to: Timestamp,
        open: ModelPrice,
    },
    Fix(ModelPrice),
}

impl PricePlan {
    pub fn describe(&self, tz: &TimeZone) -> String {
        match self {
            PricePlan::Insert(p) => format!("Added {}/{}", p.provider, p.model_pattern),
            PricePlan::Supersede {
                effective_to, open, ..
            } => format!(
                "{}/{} changed from {}",
                open.provider,
                open.model_pattern,
                format_timestamp(*effective_to, tz)
            ),
            PricePlan::Fix(p) => format!("Corrected {}/{}", p.provider, p.model_pattern),
        }
    }
}

pub fn plan_save(
    kind: SaveKind,
    editing: Option<&ModelPrice>,
    mut new: ModelPrice,
) -> Result<PricePlan, String> {
    new.id = None;
    new.source = PriceSource::User;
    new.effective_to = None;
    let user_row = || -> Result<(&ModelPrice, PriceId), String> {
        let row = editing.ok_or("no row is being edited")?;
        if row.source != PriceSource::User {
            return Err("only user rows can be changed; save a new override instead".into());
        }
        let id = row.id.ok_or("the edited row has no id")?;
        Ok((row, id))
    };
    match kind {
        SaveKind::New => Ok(PricePlan::Insert(new)),
        SaveKind::PriceChanged => {
            let (row, id) = user_row()?;
            let at = new.effective_from;
            if at <= row.effective_from {
                return Err(
                    "the new price must start after the current row's start; use \"Fix this row\" to correct the row instead"
                        .into(),
                );
            }
            if row.effective_to.is_some_and(|to| at >= to) {
                return Err("the row already ends before that date".into());
            }
            new.effective_to = row.effective_to;
            Ok(PricePlan::Supersede {
                close: id,
                effective_to: at,
                open: new,
            })
        }
        SaveKind::FixRow => {
            let (row, id) = user_row()?;
            if row.effective_to.is_some_and(|to| to <= new.effective_from) {
                return Err("effective from must be before the row's end".into());
            }
            new.id = Some(id);
            new.effective_to = row.effective_to;
            Ok(PricePlan::Fix(new))
        }
    }
}

pub fn can_close(row: &ModelPrice, now: Timestamp) -> bool {
    row.source == PriceSource::User
        && row.id.is_some()
        && row.effective_from < now
        && row.effective_to.is_none_or(|to| to > now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn form() -> PriceForm {
        PriceForm {
            provider: "anthropic".into(),
            model_pattern: "claude-opus-5-5".into(),
            input: "5".into(),
            output: "25".into(),
            cache_write_5m: "6.25".into(),
            cache_write_1h: "".into(),
            cache_read: "0.075".into(),
            effective_from: EffectiveFrom::At("2026-11-01".into()),
            long_context: None,
        }
    }

    #[test]
    fn decimal_parsing_is_exact_and_strict() {
        let x = parse_decimal("0.075").unwrap();
        assert_eq!(x, Decimal::new(75, 3));
        assert_eq!(x.to_string(), "0.075");
        assert_eq!(parse_decimal(" 3 ").unwrap(), Decimal::from(3));
        assert_eq!(parse_decimal(".5").unwrap(), d("0.5"));
        assert_eq!(parse_decimal("5.").unwrap(), Decimal::from(5));
        assert_eq!(parse_decimal("0").unwrap(), Decimal::ZERO);
        assert_eq!(
            parse_decimal("0.1").unwrap() + parse_decimal("0.2").unwrap(),
            d("0.3")
        );

        assert_eq!(parse_decimal(""), Err(NumberError::Empty));
        assert_eq!(parse_decimal("-1"), Err(NumberError::Negative));
        assert_eq!(parse_decimal("-0.5"), Err(NumberError::Negative));
        for bad in [
            "1e400", "1e3", "1E-3", "inf", "NaN", "+1", "1,5", "abc", "1.2.3", ".", "0x10",
            "1 000", "$3",
        ] {
            assert_eq!(parse_decimal(bad), Err(NumberError::Invalid), "{bad}");
        }
        assert_eq!(
            parse_decimal("123456789012345678901234567890"),
            Err(NumberError::TooPrecise)
        );
        assert_eq!(
            parse_decimal("99999999999999999999999999999"),
            Err(NumberError::TooPrecise)
        );
    }

    #[test]
    fn token_threshold_parsing() {
        assert_eq!(parse_tokens("200000"), Ok(200_000));
        assert_eq!(parse_tokens("200_000"), Ok(200_000));
        assert_eq!(parse_tokens("200,000"), Ok(200_000));
        assert_eq!(parse_tokens("2e5"), Err(NumberError::Invalid));
        assert_eq!(parse_tokens("-1"), Err(NumberError::Negative));
        assert_eq!(parse_tokens("1.5"), Err(NumberError::Invalid));
    }

    #[test]
    fn effective_from_parsing() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        assert_eq!(
            parse_timestamp("2026-11-01", &berlin),
            Ok(ts("2026-10-31T23:00:00Z"))
        );
        assert_eq!(
            parse_timestamp("2026-11-01", &TimeZone::UTC),
            Ok(ts("2026-11-01T00:00:00Z"))
        );
        assert_eq!(
            parse_timestamp("2026-11-01 14:30", &TimeZone::UTC),
            Ok(ts("2026-11-01T14:30:00Z"))
        );
        assert_eq!(
            parse_timestamp("2026-11-01T14:30:00Z", &berlin),
            Ok(ts("2026-11-01T14:30:00Z"))
        );
        assert!(parse_timestamp("yesterday", &berlin).is_err());
        assert!(parse_timestamp("", &berlin).is_err());
        assert_eq!(
            format_timestamp(ts("2026-10-31T23:00:00Z"), &berlin),
            "2026-11-01"
        );
        assert_eq!(
            format_timestamp(ts("2026-11-01T14:30:00Z"), &TimeZone::UTC),
            "2026-11-01 14:30"
        );
    }

    #[test]
    fn builds_model_price() {
        let p = form()
            .build(ts("2026-11-22T20:00:00Z"), &TimeZone::UTC)
            .unwrap();
        assert_eq!(p.id, None);
        assert_eq!(p.source, PriceSource::User);
        assert_eq!(p.provider, "anthropic");
        assert_eq!(p.model_pattern, "claude-opus-5-5");
        assert_eq!(p.effective_from, ts("2026-11-01T00:00:00Z"));
        assert_eq!(p.effective_to, None);
        assert_eq!(p.input_per_mtok, Decimal::from(5));
        assert_eq!(p.cache_write_5m_per_mtok, Some(d("6.25")));
        assert_eq!(p.cache_write_1h_per_mtok, None);
        assert_eq!(p.cache_read_per_mtok.unwrap().to_string(), "0.075");
        assert!(p.long_context.is_none());

        let mut f = form();
        f.effective_from = EffectiveFrom::Now;
        f.model_pattern = " claude-opus-5* ".into();
        f.long_context = Some(LongContextForm::default());
        let now = ts("2026-11-22T20:00:00Z");
        let p = f.build(now, &TimeZone::UTC).unwrap();
        assert_eq!(p.effective_from, now);
        assert_eq!(p.model_pattern, "claude-opus-5*");
        let lc = p.long_context.unwrap();
        assert_eq!(lc.threshold_tokens, 200_000);
        assert_eq!(lc.output, d("1.5"));
    }

    #[test]
    fn form_round_trips_through_price() {
        let tz = TimeZone::UTC;
        let mut f = form();
        f.long_context = Some(LongContextForm::default());
        let p = f.build(ts("2026-11-22T20:00:00Z"), &tz).unwrap();
        assert_eq!(PriceForm::from_price(&p, &tz), f);
    }

    #[test]
    fn form_reports_every_bad_field() {
        let mut f = form();
        f.provider = " ".into();
        f.input = "1e400".into();
        f.output = "-2".into();
        f.cache_read = "abc".into();
        f.effective_from = EffectiveFrom::At("soon".into());
        f.long_context = Some(LongContextForm {
            threshold: "0".into(),
            output: "".into(),
            ..LongContextForm::default()
        });
        let errors = f
            .build(ts("2026-11-22T20:00:00Z"), &TimeZone::UTC)
            .unwrap_err();
        let fields: Vec<Field> = errors.iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            vec![
                Field::Provider,
                Field::Input,
                Field::Output,
                Field::CacheRead,
                Field::EffectiveFrom,
                Field::LcThreshold,
                Field::LcOutput,
            ]
        );
        assert_eq!(errors[2].to_string(), "Output $/Mtok: must not be negative");
    }

    #[test]
    fn form_rejects_bad_glob() {
        let mut f = form();
        f.model_pattern = "claude-[opus".into();
        let errors = f
            .build(ts("2026-11-22T20:00:00Z"), &TimeZone::UTC)
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, Field::Pattern);
    }

    #[test]
    fn unpriced_prefill_starts_on_first_seen_day() {
        let f = PriceForm::for_model(
            "anthropic",
            "claude-haiku-5-5",
            ts("2026-11-05T13:14:15Z"),
            &TimeZone::UTC,
        );
        assert_eq!(f.model_pattern, "claude-haiku-5-5");
        assert_eq!(f.effective_from, EffectiveFrom::At("2026-11-05".into()));
        assert_eq!(f.input, "");
    }

    fn user_row(from: &str, to: Option<&str>) -> ModelPrice {
        let mut p = form().build(ts(from), &TimeZone::UTC).unwrap();
        p.id = Some(PriceId(42));
        p.effective_from = ts(from);
        p.effective_to = to.map(ts);
        p
    }

    fn new_price(from: &str, input: &str) -> ModelPrice {
        let mut f = form();
        f.input = input.into();
        f.effective_from = EffectiveFrom::At(from.into());
        f.build(ts("2026-11-22T20:00:00Z"), &TimeZone::UTC).unwrap()
    }

    #[test]
    fn new_override_is_inserted_open() {
        let plan = plan_save(SaveKind::New, None, new_price("2026-11-10", "7")).unwrap();
        let PricePlan::Insert(p) = plan else {
            panic!("expected insert")
        };
        assert_eq!(p.effective_to, None);
        assert_eq!(p.source, PriceSource::User);
    }

    #[test]
    fn price_change_closes_old_and_opens_new() {
        let row = user_row("2026-11-01T00:00:00Z", None);
        let plan = plan_save(
            SaveKind::PriceChanged,
            Some(&row),
            new_price("2026-11-10", "7"),
        )
        .unwrap();
        let PricePlan::Supersede {
            close,
            effective_to,
            open,
        } = plan
        else {
            panic!("expected supersede")
        };
        assert_eq!(close, PriceId(42));
        assert_eq!(effective_to, ts("2026-11-10T00:00:00Z"));
        assert_eq!(open.effective_from, effective_to);
        assert_eq!(open.effective_to, None);
        assert_eq!(open.id, None);
        assert_eq!(open.input_per_mtok, Decimal::from(7));
    }

    #[test]
    fn price_change_keeps_old_end_and_needs_later_start() {
        let row = user_row("2026-11-01T00:00:00Z", Some("2026-11-20T00:00:00Z"));
        let PricePlan::Supersede { open, .. } = plan_save(
            SaveKind::PriceChanged,
            Some(&row),
            new_price("2026-11-10", "7"),
        )
        .unwrap() else {
            panic!("expected supersede")
        };
        assert_eq!(open.effective_to, Some(ts("2026-11-20T00:00:00Z")));

        let same_start = plan_save(
            SaveKind::PriceChanged,
            Some(&row),
            new_price("2026-11-01", "7"),
        );
        assert!(same_start.unwrap_err().contains("Fix this row"));
        assert!(
            plan_save(
                SaveKind::PriceChanged,
                Some(&row),
                new_price("2026-11-25", "7")
            )
            .is_err()
        );
    }

    #[test]
    fn fix_rewrites_row_in_place() {
        let row = user_row("2026-11-01T00:00:00Z", Some("2026-11-20T00:00:00Z"));
        let plan = plan_save(SaveKind::FixRow, Some(&row), new_price("2026-10-15", "7")).unwrap();
        let PricePlan::Fix(p) = plan else {
            panic!("expected fix")
        };
        assert_eq!(p.id, Some(PriceId(42)));
        assert_eq!(p.effective_from, ts("2026-10-15T00:00:00Z"));
        assert_eq!(p.effective_to, Some(ts("2026-11-20T00:00:00Z")));
        assert!(plan_save(SaveKind::FixRow, Some(&row), new_price("2026-11-21", "7")).is_err());
    }

    #[test]
    fn only_user_rows_can_be_changed() {
        let mut row = user_row("2026-11-01T00:00:00Z", None);
        row.source = PriceSource::Bundled;
        let p = new_price("2026-11-10", "7");
        assert!(plan_save(SaveKind::FixRow, Some(&row), p.clone()).is_err());
        assert!(plan_save(SaveKind::PriceChanged, Some(&row), p.clone()).is_err());
        assert!(plan_save(SaveKind::FixRow, None, p).is_err());
    }

    #[test]
    fn closable_rows() {
        let now = ts("2026-11-22T20:00:00Z");
        assert!(can_close(&user_row("2026-11-01T00:00:00Z", None), now));
        assert!(!can_close(
            &user_row("2026-11-01T00:00:00Z", Some("2026-11-10T00:00:00Z")),
            now
        ));
        assert!(!can_close(&user_row("2026-11-23T00:00:00Z", None), now));
        let mut bundled = user_row("2026-11-01T00:00:00Z", None);
        bundled.source = PriceSource::Bundled;
        assert!(!can_close(&bundled, now));
    }
}
