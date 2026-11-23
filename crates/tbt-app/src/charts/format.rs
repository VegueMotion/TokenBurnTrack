use tbt_core::Micros;

pub fn micros_to_usd(m: Micros) -> f64 {
    m.0 as f64 / Micros::PER_USD as f64
}

pub fn fmt_micros(m: Micros) -> String {
    fmt_usd(micros_to_usd(m))
}

pub fn fmt_usd(usd: f64) -> String {
    if !usd.is_finite() {
        return "–".to_owned();
    }
    let sign = if usd < 0.0 { "-" } else { "" };
    let a = usd.abs();
    if a == 0.0 {
        "$0.00".to_owned()
    } else if a < 0.005 {
        format!("{sign}<$0.01")
    } else if a < 1000.0 {
        format!("{sign}${a:.2}")
    } else {
        format!("{sign}${}", compact(a))
    }
}

pub fn fmt_usd_tick(usd: f64, step: f64) -> String {
    let a = usd.abs();
    let sign = if usd < 0.0 { "-" } else { "" };
    if a == 0.0 {
        "$0".to_owned()
    } else if a >= 1000.0 {
        format!("{sign}${}", compact(a))
    } else if step >= 1.0 {
        format!("{sign}${a:.0}")
    } else if step >= 0.1 {
        format!("{sign}${a:.1}")
    } else {
        format!("{sign}${a:.2}")
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        compact(n as f64)
    }
}

pub fn fmt_count(n: u64) -> String {
    if n < 10_000 {
        n.to_string()
    } else {
        compact(n as f64)
    }
}

pub fn fmt_pct(ratio: f64) -> String {
    if ratio.is_finite() {
        format!("{:.1}%", ratio * 100.0)
    } else {
        "–".to_owned()
    }
}

pub fn fmt_duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

pub fn compact(v: f64) -> String {
    const UNITS: [(f64, &str); 4] = [(1e3, "k"), (1e6, "M"), (1e9, "B"), (1e12, "T")];
    let a = v.abs();
    if a < 1000.0 {
        return trim_zero(format!("{v:.1}"));
    }
    let sign = if v < 0.0 { "-" } else { "" };
    let mut idx = UNITS.iter().rposition(|(base, _)| a >= *base).unwrap_or(0);
    let mut scaled = (a / UNITS[idx].0 * 10.0).round() / 10.0;
    if scaled >= 1000.0 && idx + 1 < UNITS.len() {
        idx += 1;
        scaled = (a / UNITS[idx].0 * 10.0).round() / 10.0;
    }
    format!(
        "{sign}{}{}",
        trim_zero(format!("{scaled:.1}")),
        UNITS[idx].1
    )
}

fn trim_zero(s: String) -> String {
    match s.strip_suffix(".0") {
        Some(t) => t.to_owned(),
        None => s,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Usd,
    Tokens,
    Count,
}

impl ValueKind {
    pub fn fmt(self, v: f64) -> String {
        match self {
            ValueKind::Usd => fmt_usd(v),
            ValueKind::Tokens => fmt_tokens(v.max(0.0).round() as u64),
            ValueKind::Count => fmt_count(v.max(0.0).round() as u64),
        }
    }

    pub fn fmt_tick(self, v: f64, step: f64) -> String {
        match self {
            ValueKind::Usd => fmt_usd_tick(v, step),
            ValueKind::Tokens | ValueKind::Count if v.abs() < 1000.0 => {
                trim_zero(format!("{v:.1}"))
            }
            ValueKind::Tokens | ValueKind::Count => compact(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd() {
        assert_eq!(fmt_usd(1.234), "$1.23");
        assert_eq!(fmt_usd(0.0), "$0.00");
        assert_eq!(fmt_usd(0.001), "<$0.01");
        assert_eq!(fmt_usd(999.994), "$999.99");
        assert_eq!(fmt_usd(1234.0), "$1.2k");
        assert_eq!(fmt_usd(-2.5), "-$2.50");
        assert_eq!(fmt_micros(Micros(1_230_000)), "$1.23");
    }

    #[test]
    fn usd_ticks_follow_step() {
        assert_eq!(fmt_usd_tick(0.0, 5.0), "$0");
        assert_eq!(fmt_usd_tick(10.0, 5.0), "$10");
        assert_eq!(fmt_usd_tick(0.5, 0.5), "$0.5");
        assert_eq!(fmt_usd_tick(0.25, 0.05), "$0.25");
        assert_eq!(fmt_usd_tick(2500.0, 500.0), "$2.5k");
    }

    #[test]
    fn tokens() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1000), "1k");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(4_500_000), "4.5M");
        assert_eq!(fmt_tokens(999_950), "1M");
        assert_eq!(fmt_tokens(1_234_567_890), "1.2B");
    }

    #[test]
    fn pct_and_duration() {
        assert_eq!(fmt_pct(0.8734), "87.3%");
        assert_eq!(fmt_pct(f64::NAN), "–");
        assert_eq!(fmt_duration(30), "30s");
        assert_eq!(fmt_duration(45 * 60), "45m");
        assert_eq!(fmt_duration(3600 + 23 * 60 + 5), "1h 23m");
        assert_eq!(fmt_duration(90_000), "1d 1h");
    }

    #[test]
    fn value_kind_ticks() {
        assert_eq!(ValueKind::Tokens.fmt_tick(0.0, 50_000.0), "0");
        assert_eq!(ValueKind::Tokens.fmt_tick(150_000.0, 50_000.0), "150k");
        assert_eq!(ValueKind::Count.fmt_tick(2.5, 0.5), "2.5");
        assert_eq!(ValueKind::Usd.fmt(12.0), "$12.00");
    }
}
