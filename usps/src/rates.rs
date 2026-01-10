//! Historical postal rate data and lookup functions

use anyhow::{Context, Result};
use chrono::NaiveDate;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const RATES_DIR: &str = "enrichment/rates";

/// Historical rate data for a specific rate type
#[derive(Debug, Clone)]
pub struct RateHistory {
    /// Rate type name (e.g., "Letter")
    pub _name: String,
    /// Sorted list of (effective_date, rate) pairs
    rates: Vec<(NaiveDate, f64)>,
}

impl RateHistory {
    /// Load rate history from a CONL file
    pub fn load(name: &str) -> Result<Self> {
        let filename = format!("{}.conl", name.to_lowercase());
        let path = Path::new(RATES_DIR).join(&filename);
        Self::load_from_path(name, &path)
    }

    /// Load rate history from a specific path
    pub fn load_from_path(name: &str, path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read rate file: {}", path.display()))?;

        let entries: BTreeMap<String, f64> = serde_conl::from_str(&content)
            .with_context(|| format!("Failed to parse rate file: {}", path.display()))?;

        let mut rates: Vec<(NaiveDate, f64)> = entries
            .into_iter()
            .filter_map(|(date_str, rate)| {
                let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").ok()?;
                Some((date, rate))
            })
            .collect();

        // Sort by date (earliest first)
        rates.sort_by_key(|(date, _)| *date);

        Ok(Self {
            _name: name.to_string(),
            rates,
        })
    }

    /// Get the effective rate for a given date
    ///
    /// Returns the rate that was in effect on the given date,
    /// or None if the date is before the first rate entry.
    pub fn rate_on_date(&self, date: NaiveDate) -> Option<f64> {
        // Find the last rate entry that starts on or before the given date
        let mut effective_rate = None;
        for (effective_date, rate) in &self.rates {
            if *effective_date <= date {
                effective_rate = Some(*rate);
            } else {
                break;
            }
        }
        effective_rate
    }

    /// Get the effective rate for a date string in ISO format (YYYY-MM-DD)
    pub fn rate_on_date_str(&self, date_str: &str) -> Option<f64> {
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
        self.rate_on_date(date)
    }
}

/// Collection of all postal rate histories
#[derive(Debug, Clone)]
pub struct PostalRates {
    pub letter: RateHistory,
    pub ounce: RateHistory,
    pub postcard: RateHistory,
}

impl PostalRates {
    /// Load all rate histories from the rates directory
    pub fn load() -> Result<Self> {
        Ok(Self {
            letter: RateHistory::load("letter")?,
            ounce: RateHistory::load("ounce")?,
            postcard: RateHistory::load("postcard")?,
        })
    }

    /// Get the 2oz letter rate for a given date (1oz + additional ounce)
    pub fn letter_2oz(&self, date: NaiveDate) -> Option<f64> {
        let base = self.letter.rate_on_date(date)?;
        let additional = self.ounce.rate_on_date(date)?;
        Some(base + additional)
    }

    /// Get the 3oz letter rate for a given date (1oz + 2 additional ounces)
    pub fn letter_3oz(&self, date: NaiveDate) -> Option<f64> {
        let base = self.letter.rate_on_date(date)?;
        let additional = self.ounce.rate_on_date(date)?;
        Some(base + additional * 2.0)
    }

    /// Get the postcard rate for a given date
    pub fn postcard(&self, date: NaiveDate) -> Option<f64> {
        self.postcard.rate_on_date(date)
    }

    /// Get the 2oz letter rate for a date string in ISO format (YYYY-MM-DD)
    pub fn letter_2oz_str(&self, date_str: &str) -> Option<f64> {
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
        self.letter_2oz(date)
    }

    /// Get the 3oz letter rate for a date string in ISO format (YYYY-MM-DD)
    pub fn letter_3oz_str(&self, date_str: &str) -> Option<f64> {
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
        self.letter_3oz(date)
    }

    /// Get the postcard rate for a date string in ISO format (YYYY-MM-DD)
    pub fn postcard_str(&self, date_str: &str) -> Option<f64> {
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
        self.postcard(date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_history_loading() {
        // This test requires the actual file to exist
        if let Ok(history) = RateHistory::load("letter") {
            assert_eq!(history._name, "letter");
            assert!(!history.rates.is_empty());

            // Test a known rate: July 13, 2025 should be $0.78
            let date = NaiveDate::from_ymd_opt(2025, 7, 14).unwrap();
            assert_eq!(history.rate_on_date(date), Some(0.78));

            // Test a date before all rates
            let early_date = NaiveDate::from_ymd_opt(1800, 1, 1).unwrap();
            assert_eq!(history.rate_on_date(early_date), None);
        }
    }

    fn approx_eq(a: Option<f64>, b: f64) -> bool {
        match a {
            Some(v) => (v - b).abs() < 0.001,
            None => false,
        }
    }

    #[test]
    fn test_postal_rates_loading() {
        if let Ok(rates) = PostalRates::load() {
            // Test 2025 rates (effective July 13, 2025)
            let date = NaiveDate::from_ymd_opt(2025, 7, 14).unwrap();

            // Letter 2oz: $0.78 + $0.29 = $1.07
            assert!(approx_eq(rates.letter_2oz(date), 1.07));

            // Letter 3oz: $0.78 + $0.29*2 = $1.36
            assert!(approx_eq(rates.letter_3oz(date), 1.36));

            // Postcard: $0.61
            assert!(approx_eq(rates.postcard(date), 0.61));
        }
    }
}
