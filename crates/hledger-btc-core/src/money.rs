use std::cmp::Ordering;
use std::fmt;
use std::ops::Neg;
use std::str::FromStr;
use anyhow::Result;
use bigdecimal::{BigDecimal, ToPrimitive, Zero};

/// A signed amount in a specific commodity.
#[derive(Debug, Clone)]
pub struct Money {
    pub amount: BigDecimal,
    pub commodity: String,
}

impl Money {
    pub fn sat(amount: i64) -> Self {
        Money { amount: BigDecimal::from(amount), commodity: "SAT".to_string() }
    }

    pub fn new(amount: BigDecimal, commodity: impl Into<String>) -> Self {
        Money { amount, commodity: commodity.into() }
    }

    /// Parse a decimal string (e.g. from an API) into a Money with the given commodity.
    pub fn parse(s: &str, commodity: impl Into<String>) -> Result<Self> {
        let amount = BigDecimal::from_str(s.trim())
            .map_err(|e| anyhow::anyhow!("invalid amount {:?}: {e}", s))?;
        Ok(Money { amount, commodity: commodity.into() })
    }

}

impl Money {
    pub fn is_zero(&self) -> bool { self.amount.is_zero() }
    pub fn is_negative(&self) -> bool { self.amount < BigDecimal::zero() }
    pub fn is_positive(&self) -> bool { self.amount > BigDecimal::zero() }
}

impl PartialEq for Money {
    fn eq(&self, other: &Self) -> bool {
        self.commodity == other.commodity && self.amount == other.amount
    }
}

impl PartialOrd for Money {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.commodity != other.commodity { return None; }
        self.amount.partial_cmp(&other.amount)
    }
}

impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money { amount: -self.amount, commodity: self.commodity }
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.commodity.as_str() {
            "SAT" => {
                let sats = self.amount.to_i64().unwrap_or(0);
                write!(f, "{} sat", fmt_sats(sats))
            }
            "USD" => {
                let rounded = self.amount.abs().with_scale_round(2, bigdecimal::RoundingMode::HalfUp);
                if self.amount < BigDecimal::zero() {
                    write!(f, "-${rounded}")
                } else {
                    write!(f, "${rounded}")
                }
            }
            other => write!(f, "{} {other}", self.amount),
        }
    }
}

fn fmt_sats(sats: i64) -> String {
    let s = sats.unsigned_abs().to_string();
    let with_commas = s.as_bytes().rchunks(3)
        .rev()
        .map(std::str::from_utf8)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join(",");
    if sats < 0 { format!("-{with_commas}") } else { with_commas }
}
