use serde::{Deserialize, Serialize};

/// A frame rate as ffmpeg writes it: `30000/1001`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: u64,
    pub den: u64,
}

impl Rational {
    pub const fn new(num: u64, den: u64) -> Self {
        Self { num, den }
    }

    /// The same rate in lowest terms.
    pub fn reduced(self) -> Self {
        let (mut a, mut b) = (self.num, self.den);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        match (self.num.checked_div(a), self.den.checked_div(a)) {
            (Some(num), Some(den)) => Self::new(num, den),
            _ => self,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (num, den) = text.trim().split_once('/').unwrap_or((text.trim(), "1"));
        let rational = Self::new(num.parse().ok()?, den.parse().ok()?);
        (rational.den > 0).then_some(rational)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// The broadcast rate closest to `self` (screen recorders report rates like
    /// 304837120/5080417 for "60"), or `self` rounded to a whole number.
    pub fn standard(self) -> Self {
        const RATES: [Rational; 9] = [
            Rational::new(24000, 1001),
            Rational::new(24, 1),
            Rational::new(25, 1),
            Rational::new(30000, 1001),
            Rational::new(30, 1),
            Rational::new(48, 1),
            Rational::new(50, 1),
            Rational::new(60000, 1001),
            Rational::new(60, 1),
        ];
        let value = self.as_f64();
        let nearest =
            RATES.into_iter().min_by(|a, b| (a.as_f64() - value).abs().total_cmp(&(b.as_f64() - value).abs()));
        match nearest {
            Some(rate) if (rate.as_f64() - value).abs() / rate.as_f64() < 0.006 => rate,
            _ => Self::new(value.round().clamp(1.0, 240.0) as u64, 1),
        }
    }
}

impl std::fmt::Display for Rational {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_screen_recorder_rates_become_standard_rates() {
        assert_eq!(Rational::parse("304837120/5080417").unwrap().standard(), Rational::new(60, 1));
        assert_eq!(Rational::parse("2997/100").unwrap().standard(), Rational::new(30000, 1001));
        assert_eq!(Rational::parse("15").unwrap().standard(), Rational::new(15, 1));
        assert_eq!(Rational::parse("0/0"), None);
    }
}
