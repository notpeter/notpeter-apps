//! Stamp metadata types with CONL serialization support

use serde::{Deserialize, Serialize};

/// Rate type for stamps (determines pricing structure)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateType {
    Forever,
    Postcard,
    International,
    #[serde(rename = "Global Forever")]
    GlobalForever,
    #[serde(rename = "Additional Ounce")]
    AdditionalOunce,
    #[serde(rename = "Two Ounce")]
    TwoOunce,
    #[serde(rename = "Three Ounce")]
    ThreeOunce,
    #[serde(rename = "Nonmachineable Surcharge")]
    Nonmachineable,
    Semipostal,
    Definitive,
    #[serde(rename = "Priority Mail")]
    PriorityMail,
    #[serde(rename = "Priority Mail Express")]
    PriorityMailExpress,
    #[serde(rename = "Presorted First-Class")]
    PresortedFirstClass,
    #[serde(rename = "Presorted Standard")]
    PresortedStandard,
    Nonprofit,
    #[serde(other)]
    Other,
}

impl RateType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RateType::Forever => "Forever",
            RateType::Postcard => "Postcard",
            RateType::International => "International",
            RateType::GlobalForever => "Global Forever",
            RateType::AdditionalOunce => "Additional Ounce",
            RateType::TwoOunce => "Two Ounce",
            RateType::ThreeOunce => "Three Ounce",
            RateType::Nonmachineable => "Nonmachineable Surcharge",
            RateType::Semipostal => "Semipostal",
            RateType::Definitive => "Definitive",
            RateType::PriorityMail => "Priority Mail",
            RateType::PriorityMailExpress => "Priority Mail Express",
            RateType::PresortedFirstClass => "Presorted First-Class",
            RateType::PresortedStandard => "Presorted Standard",
            RateType::Nonprofit => "Nonprofit",
            RateType::Other => "Other",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "Forever" => RateType::Forever,
            "Postcard" => RateType::Postcard,
            "International" => RateType::International,
            "Global Forever" => RateType::GlobalForever,
            "Additional Ounce" | "Additional Postage" => RateType::AdditionalOunce,
            "Two Ounce" => RateType::TwoOunce,
            "Three Ounce" => RateType::ThreeOunce,
            "Nonmachineable Surcharge" => RateType::Nonmachineable,
            "Semipostal" => RateType::Semipostal,
            "Definitive" => RateType::Definitive,
            "Priority Mail" => RateType::PriorityMail,
            "Priority Mail Express" => RateType::PriorityMailExpress,
            "Presorted First-Class" => RateType::PresortedFirstClass,
            "Presorted Standard" => RateType::PresortedStandard,
            "Nonprofit" => RateType::Nonprofit,
            _ => RateType::Other,
        }
    }

    /// Returns true if this rate type represents a "forever" stamp
    pub fn is_forever(&self) -> bool {
        matches!(
            self,
            RateType::Forever
                | RateType::Postcard
                | RateType::International
                | RateType::GlobalForever
                | RateType::AdditionalOunce
                | RateType::TwoOunce
                | RateType::ThreeOunce
                | RateType::Nonmachineable
                | RateType::Semipostal
        )
    }
}

/// Type of postal item
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StampType {
    #[default]
    Stamp,
    Card,
    Envelope,
}

impl StampType {
    pub fn as_str(&self) -> &'static str {
        match self {
            StampType::Stamp => "stamp",
            StampType::Card => "card",
            StampType::Envelope => "envelope",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "card" => StampType::Card,
            "envelope" => StampType::Envelope,
            _ => StampType::Stamp,
        }
    }
}

/// Credits for a stamp (art director, designer, etc.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub art_director: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub designer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typographer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photographer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub illustrator: Option<String>,
}

impl Credits {
    pub fn is_empty(&self) -> bool {
        self.art_director.is_none()
            && self.artist.is_none()
            && self.designer.is_none()
            && self.typographer.is_none()
            && self.photographer.is_none()
            && self.illustrator.is_none()
    }
}

/// Product listing for a stamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postal_store_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamps_forever_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Parsed product metadata (envelope size, style, closure, quantity)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Complete stamp metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StampMetadata {
    pub name: String,
    pub slug: String,
    pub api_slug: String,
    pub url: String,
    pub year: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_location: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_type: Option<RateType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_cost: Option<f64>,

    pub forever: bool,

    #[serde(rename = "type")]
    pub stamp_type: StampType,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamp_images: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet_image: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,

    #[serde(default, skip_serializing_if = "Credits::is_empty")]
    pub credits: Credits,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub products: Vec<Product>,
}
