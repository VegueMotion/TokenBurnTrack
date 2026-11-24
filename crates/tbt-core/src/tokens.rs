use std::iter::Sum;
use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// Token counts of one call, or a running total. Same semantics as the fields of
/// [`crate::UsageEvent`]: `reasoning` is a subset of `output`, so [`total`](Self::total)
/// does not add it again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub reasoning: u64,
}

impl TokenCounts {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write() + self.cache_read
    }

    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read
    }

    pub fn is_zero(&self) -> bool {
        *self == TokenCounts::default()
    }

    pub fn checked_sub(&self, other: &TokenCounts) -> Option<TokenCounts> {
        Some(TokenCounts {
            input: self.input.checked_sub(other.input)?,
            output: self.output.checked_sub(other.output)?,
            cache_write_5m: self.cache_write_5m.checked_sub(other.cache_write_5m)?,
            cache_write_1h: self.cache_write_1h.checked_sub(other.cache_write_1h)?,
            cache_read: self.cache_read.checked_sub(other.cache_read)?,
            reasoning: self.reasoning.checked_sub(other.reasoning)?,
        })
    }

    pub fn delta_since(&self, prev: &TokenCounts) -> TokenCounts {
        self.checked_sub(prev).unwrap_or(*self)
    }
}

impl Add for TokenCounts {
    type Output = TokenCounts;
    fn add(mut self, rhs: TokenCounts) -> TokenCounts {
        self += rhs;
        self
    }
}

impl AddAssign for TokenCounts {
    fn add_assign(&mut self, rhs: TokenCounts) {
        self.input += rhs.input;
        self.output += rhs.output;
        self.cache_write_5m += rhs.cache_write_5m;
        self.cache_write_1h += rhs.cache_write_1h;
        self.cache_read += rhs.cache_read;
        self.reasoning += rhs.reasoning;
    }
}

impl Sum for TokenCounts {
    fn sum<I: Iterator<Item = TokenCounts>>(iter: I) -> TokenCounts {
        iter.fold(TokenCounts::default(), Add::add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(input: u64, output: u64, cr: u64) -> TokenCounts {
        TokenCounts {
            input,
            output,
            cache_read: cr,
            ..Default::default()
        }
    }

    #[test]
    fn totals_exclude_reasoning() {
        let t = TokenCounts {
            input: 10,
            output: 20,
            cache_write_5m: 1,
            cache_write_1h: 2,
            cache_read: 100,
            reasoning: 15,
        };
        assert_eq!(t.total(), 133);
        assert_eq!(t.cache_write(), 3);
        assert_eq!(t.prompt(), 113);
    }

    #[test]
    fn delta_and_reset() {
        let prev = tc(100, 50, 1000);
        let now = tc(150, 60, 1500);
        assert_eq!(now.checked_sub(&prev), Some(tc(50, 10, 500)));
        assert_eq!(now.delta_since(&prev), tc(50, 10, 500));
        let reset = tc(20, 70, 0);
        assert_eq!(reset.checked_sub(&prev), None);
        assert_eq!(reset.delta_since(&prev), reset);
    }

    #[test]
    fn sum() {
        let s: TokenCounts = [tc(1, 2, 3), tc(4, 5, 6)].into_iter().sum();
        assert_eq!(s, tc(5, 7, 9));
        assert!(TokenCounts::default().is_zero());
    }
}
