use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// Money as integer micro-USD (USD × 1e6).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Micros(pub i64);

impl Micros {
    pub const ZERO: Micros = Micros(0);
    pub const PER_USD: i64 = 1_000_000;

    /// Rounds to the nearest micro (midpoint away from zero); saturates at the i64 range.
    pub fn from_usd(usd: Decimal) -> Micros {
        match usd.checked_mul(Decimal::from(Self::PER_USD)) {
            Some(micros) => Self::from_micros_decimal(micros),
            None if usd.is_sign_negative() => Micros(i64::MIN),
            None => Micros(i64::MAX),
        }
    }

    /// For values already expressed in micro-USD, e.g. `tokens × usd_per_mtok`.
    pub fn from_micros_decimal(micros: Decimal) -> Micros {
        let rounded = micros.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        match rounded.to_i64() {
            Some(v) => Micros(v),
            None if rounded.is_sign_negative() => Micros(i64::MIN),
            None => Micros(i64::MAX),
        }
    }

    pub fn to_usd(self) -> Decimal {
        Decimal::new(self.0, 6)
    }

    pub fn saturating_add(self, rhs: Micros) -> Micros {
        Micros(self.0.saturating_add(rhs.0))
    }
}

impl fmt::Display for Micros {
    /// Formats as `$1.234567`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.to_usd();
        if d.is_sign_negative() {
            write!(f, "-${}", -d)
        } else {
            write!(f, "${d}")
        }
    }
}

impl Add for Micros {
    type Output = Micros;
    fn add(self, rhs: Micros) -> Micros {
        Micros(self.0 + rhs.0)
    }
}

impl AddAssign for Micros {
    fn add_assign(&mut self, rhs: Micros) {
        self.0 += rhs.0;
    }
}

impl Sub for Micros {
    type Output = Micros;
    fn sub(self, rhs: Micros) -> Micros {
        Micros(self.0 - rhs.0)
    }
}

impl SubAssign for Micros {
    fn sub_assign(&mut self, rhs: Micros) {
        self.0 -= rhs.0;
    }
}

impl Neg for Micros {
    type Output = Micros;
    fn neg(self) -> Micros {
        Micros(-self.0)
    }
}

impl Sum for Micros {
    fn sum<I: Iterator<Item = Micros>>(iter: I) -> Micros {
        iter.fold(Micros::ZERO, Add::add)
    }
}

impl<'a> Sum<&'a Micros> for Micros {
    fn sum<I: Iterator<Item = &'a Micros>>(iter: I) -> Micros {
        iter.copied().sum()
    }
}

impl From<Micros> for Decimal {
    fn from(m: Micros) -> Decimal {
        m.to_usd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn usd_roundtrip() {
        assert_eq!(Micros::from_usd(d("1.5")), Micros(1_500_000));
        assert_eq!(Micros(1_234_567).to_usd(), d("1.234567"));
        assert_eq!(Micros::from_usd(Micros(42).to_usd()), Micros(42));
    }

    #[test]
    fn rounding_is_midpoint_away_from_zero() {
        assert_eq!(Micros::from_usd(d("0.0000005")), Micros(1));
        assert_eq!(Micros::from_usd(d("0.00000049")), Micros(0));
        assert_eq!(Micros::from_usd(d("-0.0000005")), Micros(-1));
        assert_eq!(Micros::from_micros_decimal(d("2.5")), Micros(3));
    }

    #[test]
    fn saturates_on_overflow() {
        assert_eq!(Micros::from_usd(Decimal::MAX), Micros(i64::MAX));
        assert_eq!(Micros::from_usd(Decimal::MIN), Micros(i64::MIN));
    }

    #[test]
    fn arithmetic_and_sum() {
        let v = [Micros(1), Micros(2), Micros(3)];
        assert_eq!(v.iter().sum::<Micros>(), Micros(6));
        assert_eq!(v.into_iter().sum::<Micros>(), Micros(6));
        let mut m = Micros(10);
        m += Micros(5);
        m -= Micros(3);
        assert_eq!(m, Micros(12));
        assert_eq!(-m, Micros(-12));
        assert_eq!(Micros(i64::MAX).saturating_add(Micros(1)), Micros(i64::MAX));
    }

    #[test]
    fn display() {
        assert_eq!(Micros(1_234_567).to_string(), "$1.234567");
        assert_eq!(Micros(-500_000).to_string(), "-$0.500000");
        assert_eq!(Micros::ZERO.to_string(), "$0.000000");
    }

    #[test]
    fn serde_is_integer() {
        assert_eq!(serde_json::to_string(&Micros(5)).unwrap(), "5");
    }
}
