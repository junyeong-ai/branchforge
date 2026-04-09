//! Newtype for consumed-token counts.
//!
//! `TokenCount` wraps a `u64` and is the canonical type for **consumed**
//! token fields: metrics accumulators, compaction savings, tool-index
//! estimates, etc.
//!
//! Wire-format *limit* fields (`max_output_tokens`, `max_tokens`) remain
//! plain integers because they flow directly into JSON request bodies and
//! are never accumulated.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// A consumed-token count.
///
/// Thin `u64` wrapper with saturating arithmetic and `serde(transparent)`.
/// Intended for metrics, budget tracking, compaction records, and tool
/// index estimates — anywhere we count tokens that have already been used.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TokenCount(u64);

impl TokenCount {
    /// The zero token count.
    pub const ZERO: Self = Self(0);

    /// Create a new `TokenCount` from a raw `u64`.
    #[inline]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    /// Return the inner `u64`.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Saturating subtraction.
    #[inline]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    /// Saturating addition.
    #[inline]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    /// Checked addition, returning `None` on overflow.
    #[inline]
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<u64> for TokenCount {
    #[inline]
    fn from(n: u64) -> Self {
        Self(n)
    }
}

impl From<TokenCount> for u64 {
    #[inline]
    fn from(tc: TokenCount) -> Self {
        tc.0
    }
}

// ---------------------------------------------------------------------------
// Arithmetic operators
// ---------------------------------------------------------------------------

impl Add for TokenCount {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for TokenCount {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for TokenCount {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl SubAssign for TokenCount {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl Sum for TokenCount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |a, b| Self(a.0 + b.0))
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl fmt::Display for TokenCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_constant() {
        assert_eq!(TokenCount::ZERO, TokenCount::new(0));
        assert_eq!(TokenCount::ZERO.get(), 0);
    }

    #[test]
    fn basic_arithmetic() {
        let a = TokenCount::new(100);
        let b = TokenCount::new(40);
        assert_eq!((a + b).get(), 140);
        assert_eq!((a - b).get(), 60);
    }

    #[test]
    fn add_assign_sub_assign() {
        let mut tc = TokenCount::new(50);
        tc += TokenCount::new(10);
        assert_eq!(tc.get(), 60);
        tc -= TokenCount::new(5);
        assert_eq!(tc.get(), 55);
    }

    #[test]
    fn saturating_sub_clamps_at_zero() {
        let a = TokenCount::new(10);
        let b = TokenCount::new(20);
        assert_eq!(a.saturating_sub(b), TokenCount::ZERO);
    }

    #[test]
    fn saturating_add_clamps_at_max() {
        let a = TokenCount::new(u64::MAX);
        let b = TokenCount::new(1);
        assert_eq!(a.saturating_add(b), TokenCount::new(u64::MAX));
    }

    #[test]
    fn checked_add_returns_none_on_overflow() {
        let a = TokenCount::new(u64::MAX);
        let b = TokenCount::new(1);
        assert!(a.checked_add(b).is_none());
        assert_eq!(
            TokenCount::new(10).checked_add(TokenCount::new(5)),
            Some(TokenCount::new(15))
        );
    }

    #[test]
    fn from_u64_and_back() {
        let tc = TokenCount::from(42u64);
        let v: u64 = tc.into();
        assert_eq!(v, 42);
    }

    #[test]
    fn serde_transparent_roundtrip() {
        let tc = TokenCount::new(12345);
        let json = serde_json::to_string(&tc).unwrap();
        assert_eq!(json, "12345");
        let back: TokenCount = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tc);
    }

    #[test]
    fn sum_iterator() {
        let counts = vec![
            TokenCount::new(10),
            TokenCount::new(20),
            TokenCount::new(30),
        ];
        let total: TokenCount = counts.into_iter().sum();
        assert_eq!(total, TokenCount::new(60));
    }

    #[test]
    fn display_shows_raw_number() {
        assert_eq!(format!("{}", TokenCount::new(999)), "999");
    }

    #[test]
    fn ordering() {
        let a = TokenCount::new(5);
        let b = TokenCount::new(10);
        assert!(a < b);
        assert!(b > a);
        assert_eq!(TokenCount::new(5), TokenCount::new(5));
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(TokenCount::default(), TokenCount::ZERO);
    }
}
