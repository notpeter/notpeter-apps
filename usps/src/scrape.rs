use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use scraper::Html;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::types::{Credits, Product, RateType, StampMetadata, StampType};
use crate::utils::{osc8_file_link, osc8_link};
use crate::{detect_stamp_type, init_database, parse_date_to_iso, MIN_SCRAPE_YEAR, STAMPS_API_URL};

const CACHE_DIR: &str = "cache";
const STAMPS_DIR: &str = "data/stamps";
const OVERRIDES_DIR: &str = "enrichment/stamps";

/// Override data for a stamp (loaded from enrichment/stamps/{year}.conl)
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct StampOverrides {
    rate_type: Option<String>,
    rate: Option<String>,
    issue_date: Option<String>,
    issue_location: Option<String>,
    slug: Option<String>,
    forever: Option<bool>,
    extra_cost: Option<f64>,
    issued: Option<String>,
    #[serde(rename = "type")]
    stamp_type: Option<String>,
}

/// Valid rate_type values (must match RateType enum variants)
const VALID_RATE_TYPES: &[&str] = &[
    "Forever",
    "Postcard",
    "International",
    "Global Forever",
    "Additional Ounce",
    "Additional Postage",
    "Two Ounce",
    "Three Ounce",
    "Nonmachineable Surcharge",
    "Semipostal",
    "Definitive",
    "Priority Mail",
    "Priority Mail Express",
    "Presorted First-Class",
    "Presorted Standard",
    "Nonprofit",
];

/// Load all overrides from year-based CONL files in enrichment/stamps/
fn load_overrides() -> HashMap<u32, HashMap<String, StampOverrides>> {
    let mut all_overrides: HashMap<u32, HashMap<String, StampOverrides>> = HashMap::new();

    let dir = match fs::read_dir(OVERRIDES_DIR) {
        Ok(d) => d,
        Err(_) => return all_overrides,
    };

    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "conl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(year) = stem.parse::<u32>() {
                    let content = match fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    let overrides: HashMap<String, StampOverrides> =
                        match serde_conl::from_str(&content) {
                            Ok(o) => o,
                            Err(e) => {
                                panic!("Failed to parse {}: {}", path.display(), e);
                            }
                        };

                    // Validate rate_type values
                    for (slug, stamp_override) in &overrides {
                        if let Some(ref rate_type) = stamp_override.rate_type {
                            if !VALID_RATE_TYPES.contains(&rate_type.as_str()) {
                                panic!(
                                    "Invalid rate_type '{}' for '{}' in {}. Valid values: {:?}",
                                    rate_type,
                                    slug,
                                    path.display(),
                                    VALID_RATE_TYPES
                                );
                            }
                        }
                    }

                    all_overrides.insert(year, overrides);
                }
            }
        }
    }

    all_overrides
}

// Detailed stamp API response types
#[derive(Debug, Deserialize)]
struct StampDetail {
    #[allow(dead_code)] // Deserialized but unused - we get slug from URL param
    slug: String,
    name: String,
    issue_date: Option<String>,
    issue_location: Option<String>,
    rate: Option<String>,
    rate_type: Option<String>,
    caption: Option<String>,
    about: Option<String>,
    series: Option<SeriesInfo>,
    images: Vec<ImageInfo>,
    stamp_pane: Option<ImageInfo>,
    people_groupings: Option<Vec<PeopleGrouping>>,
    product_listings: Option<Vec<ProductListing>>,
    background_color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeriesInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ImageInfo {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PeopleGrouping {
    heading: Option<String>,
    people: Vec<PersonInfo>,
}

#[derive(Debug, Deserialize)]
struct PersonInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ProductListing {
    product_number: Option<String>,
    product_title: String,
    long_title: Option<String>,
    price: Option<String>,
    postal_store_url: Option<String>,
    media: Option<Vec<ProductMedia>>,
}

#[derive(Debug, Deserialize)]
struct ProductMedia {
    path: Option<String>, // Videos have "url" instead, so this is None for them
}

// Cache system
struct CachedClient {
    client: reqwest::blocking::Client,
    cache_dir: PathBuf,
}

impl CachedClient {
    fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; USPSStampScraper/1.0)")
            .build()?;
        let cache_dir = PathBuf::from(CACHE_DIR);
        Ok(Self { client, cache_dir })
    }

    fn url_to_cache_path(&self, url: &str) -> PathBuf {
        // Strip query params and protocol, use as path
        let url = url.split('?').next().unwrap_or(url);
        if let Some(stripped) = url.strip_prefix("https://") {
            self.cache_dir.join(stripped)
        } else if let Some(stripped) = url.strip_prefix("http://") {
            self.cache_dir.join(stripped)
        } else {
            self.cache_dir.join(url)
        }
    }

    fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let cache_path = self.url_to_cache_path(url);

        if cache_path.exists() {
            let content = fs::read_to_string(&cache_path)
                .with_context(|| format!("Failed to read cache: {:?}", cache_path))?;
            return serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse cached JSON: {:?}", cache_path));
        }

        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("Failed to fetch: {}", url))?;

        let text = response
            .text()
            .with_context(|| format!("Failed to read response: {}", url))?;

        // Cache the response
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &text)?;

        serde_json::from_str(&text).with_context(|| format!("Failed to parse JSON: {}", url))
    }

    fn fetch_binary(&self, url: &str) -> Result<Vec<u8>> {
        let cache_path = self.url_to_cache_path(url);

        if cache_path.exists() {
            return fs::read(&cache_path)
                .with_context(|| format!("Failed to read cache: {:?}", cache_path));
        }

        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("Failed to fetch: {}", url))?;

        let bytes = response
            .bytes()
            .with_context(|| format!("Failed to read response: {}", url))?;

        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &bytes)?;

        Ok(bytes.to_vec())
    }
}

fn html_to_text(html: &str) -> String {
    let document = Html::parse_fragment(html);

    // Extract text from all text nodes, joining with spaces
    let text: String = document.root_element().text().collect::<Vec<_>>().join(" ");

    // Clean up: normalize whitespace and newlines
    let mut cleaned = String::new();
    let mut prev_was_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_was_space && !cleaned.is_empty() {
                cleaned.push(' ');
                prev_was_space = true;
            }
        } else {
            cleaned.push(c);
            prev_was_space = false;
        }
    }
    cleaned.trim().to_string()
}

fn is_included_product(title: &str) -> bool {
    let lower = title.to_lowercase();
    // Exclude first day covers and strips (not purchasable separately)
    if lower.contains("first day cover") || lower.contains("strip of") {
        return false;
    }
    // Include: Pane, Press Sheet, Keepsake, Notecard, Ceremony Program, Pack of 5 (envelopes), Booklet, Coil, Stamped Card
    lower.contains("pane")
        || lower.contains("press sheet")
        || lower.contains("keepsake")
        || lower.contains("notecard")
        || lower.contains("ceremony program")
        || lower.contains("stamp folio")
        || lower.contains("pack of 5")
        || lower.contains("booklet")
        || lower.contains("coil of")
        || lower.contains("stamped card")
        || lower.contains("double reply")
}

/// Clean up product title for display (expand abbreviations, remove printer codes)
fn clean_product_title(title: &str) -> String {
    title
        .replace(" PSA", " peel-and-stick")
        .replace(" WAG", " gummed")
        // Remove printer codes like (BCA), (APU) that create duplicate products
        .replace(" (BCA)", "")
        .replace(" (APU)", "")
}

/// Extract quantity from product title (e.g., "Pane of 20" -> 20, "Coil of 3,000" -> 3000)
fn extract_quantity(title: &str) -> Option<u32> {
    let lower = title.to_lowercase();

    // Look for patterns like "of 20", "of 100", "of 3,000", "of 10,000"
    for pattern in ["pane of ", "booklet of ", "coil of ", "pack of ", "box of "] {
        if let Some(start) = lower.find(pattern) {
            let num_start = start + pattern.len();
            let rest = &title[num_start..];
            // Extract digits and commas, then parse
            let num_str: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .filter(|c| c.is_ascii_digit())
                .collect();
            if !num_str.is_empty() {
                return num_str.parse().ok();
            }
        }
    }
    None
}

/// Parse product metadata from title
/// Returns a JSON object with extracted attributes, or None if not parseable
fn parse_product_metadata(title: &str) -> Option<serde_json::Value> {
    let lower = title.to_lowercase();
    let mut metadata = serde_json::Map::new();

    // Determine product format type
    let format = if lower.contains("envelope") {
        "envelope"
    } else if lower.contains("double reply") {
        "double-reply-card"
    } else if lower.contains("stamped card") {
        "stamped-card"
    } else if lower.contains("pane of") {
        "pane"
    } else if lower.contains("booklet of") {
        "booklet"
    } else if lower.contains("coil of") {
        "coil"
    } else if lower.contains("press sheet") {
        "press-sheet"
    } else {
        return None; // Unknown format, skip metadata
    };
    metadata.insert("format".to_string(), serde_json::Value::String(format.to_string()));

    // Extract quantity
    if let Some(qty) = extract_quantity(title) {
        metadata.insert("quantity".to_string(), serde_json::Value::Number(qty.into()));
    }

    // Envelope-specific metadata
    if format == "envelope" {
        // Extract size (e.g., "#6-3/4", "#9", "#10")
        if let Some(size_match) = title.find('#') {
            let rest = &title[size_match..];
            let size_end = rest
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            let size = &rest[..size_end];
            metadata.insert("size".to_string(), serde_json::Value::String(size.to_string()));
        }

        // Extract style (Regular, Window, Regular Security, Window Security)
        let style = if lower.contains("window security") {
            "window-security"
        } else if lower.contains("regular security") {
            "regular-security"
        } else if lower.contains("window") {
            "window"
        } else if lower.contains("regular") {
            "regular"
        } else {
            "unknown"
        };
        metadata.insert("style".to_string(), serde_json::Value::String(style.to_string()));

        // Extract closure type (PSA = peel-and-stick, WAG = gummed)
        let closure = if title.contains("PSA") || lower.contains("peel-and-stick") {
            "peel-and-stick"
        } else if title.contains("WAG") || lower.contains("gummed") {
            "gummed"
        } else {
            "unknown"
        };
        metadata.insert("closure".to_string(), serde_json::Value::String(closure.to_string()));
    }

    // Booklet-specific metadata
    if format == "booklet" {
        if lower.contains("2-sided") || lower.contains("two-sided") {
            metadata.insert("sided".to_string(), serde_json::Value::Number(2.into()));
        } else if lower.contains("1-sided") || lower.contains("one-sided") {
            metadata.insert("sided".to_string(), serde_json::Value::Number(1.into()));
        }
    }

    Some(serde_json::Value::Object(metadata))
}

fn extract_image_filename(url: &str) -> String {
    url.split('/')
        .last()
        .unwrap_or("image.png")
        .split('?')
        .next()
        .unwrap_or("image.png")
        .to_string()
}

/// Suffixes that should NOT cause a comma split (e.g., "Edith Widder, Ph.D." is one name)
const NAME_SUFFIXES: &[&str] = &["Ph.D.", "M.D.", "Jr.", "Sr.", "II", "III", "IV"];

const ALLOWED_SHORT_NAMES: &[&str] = &[
    "USPS",
    "NASA",
    "AP",
    "UPI",
    "the U.S. Navy",
    "U.S. Marine Corps",
    "U.S. Navy",
    "LEGO",
    "LIFE Images",
    "LIFE",
];

const KNOWN_SOURCE_HEADINGS: &[&str] = &["Walt Disney Studios Ink & Paint Department"];

/// Current USPS Forever stamp rates (updated 2025)
/// These are the rates that forever stamps are worth when used today
const CURRENT_FOREVER_RATE: f64 = 0.78; // 1oz letter
const CURRENT_TWO_OUNCE_RATE: f64 = 1.07; // 2oz letter
const CURRENT_THREE_OUNCE_RATE: f64 = 1.36; // 3oz letter
const CURRENT_ADDITIONAL_OUNCE_RATE: f64 = 0.29;
const CURRENT_POSTCARD_RATE: f64 = 0.61;
const CURRENT_GLOBAL_FOREVER_RATE: f64 = 1.70;
const CURRENT_NONMACHINABLE_RATE: f64 = 1.27; // 0.78 + 0.49 surcharge

/// Get the current rate for a stamp based on its rate_type
/// For forever stamps, returns the current day's value
/// For denominated stamps, returns the face value from API
fn get_corrected_rate(
    _api_slug: &str,
    api_rate: Option<&str>,
    rate_type: Option<&str>,
) -> Option<String> {
    // For forever stamps, return current rate based on type
    match rate_type {
        Some("Forever") | Some("Semipostal") => Some(format!("{:.2}", CURRENT_FOREVER_RATE)),
        Some("Two Ounce") => Some(format!("{:.2}", CURRENT_TWO_OUNCE_RATE)),
        Some("Three Ounce") => Some(format!("{:.2}", CURRENT_THREE_OUNCE_RATE)),
        Some("Additional Ounce") | Some("Additional Postage") => {
            Some(format!("{:.2}", CURRENT_ADDITIONAL_OUNCE_RATE))
        }
        Some("Postcard") => Some(format!("{:.2}", CURRENT_POSTCARD_RATE)),
        Some("International") | Some("Global Forever") => {
            Some(format!("{:.2}", CURRENT_GLOBAL_FOREVER_RATE))
        }
        Some("Nonmachineable Surcharge") => Some(format!("{:.2}", CURRENT_NONMACHINABLE_RATE)),
        // For denominated stamps (Definitive, etc.), use the API-provided rate
        _ => api_rate.map(|s| s.to_string()),
    }
}

#[derive(Debug)]
enum CreditsHeadingType {
    EmbeddedNames,
    Roles {
        art_director: bool,
        artist: bool,
        designer: bool,
        typographer: bool,
        photographer: bool,
        illustrator: bool,
    },
}

fn parse_credits_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    // Handle "Name1 and Name2" or "Name1, Name2, and Name3"
    let clean = text.replace(" and ", ", ").replace(" & ", ", ");

    let parts: Vec<&str> = clean.split(',').collect();
    let mut i = 0;

    while i < parts.len() {
        let mut name = parts[i].trim().to_string();

        // Check if next part is a suffix that should be combined (e.g., "Ph.D.")
        while i + 1 < parts.len() {
            let next = parts[i + 1].trim();
            if NAME_SUFFIXES.contains(&next) {
                name = format!("{}, {}", name, next);
                i += 1;
            } else {
                break;
            }
        }

        if name.len() >= 3 || ALLOWED_SHORT_NAMES.contains(&name.as_str()) {
            // Check if it looks like a name (contains space or is short org name)
            if name.contains(' ') || ALLOWED_SHORT_NAMES.contains(&name.as_str()) {
                // Skip if it's a role word
                let lower = name.to_lowercase();
                if !lower.contains("existing")
                    && !lower.contains("original")
                    && !lower.contains("photo")
                    && !lower.contains("art")
                {
                    names.push(name);
                }
            }
        }
        i += 1;
    }
    names
}

fn parse_credits_heading(heading: &str) -> CreditsHeadingType {
    let lower = heading.to_lowercase();

    // Check for embedded names pattern
    if lower.contains("existing")
        || lower.contains("original")
        || lower.contains("source")
        || KNOWN_SOURCE_HEADINGS.contains(&heading)
    {
        return CreditsHeadingType::EmbeddedNames;
    }

    let art_director = lower.contains("art director");
    let artist = lower.contains("artist") && !lower.contains("art director");
    let designer = lower.contains("designer");
    let typographer = lower.contains("typographer");
    let photographer = lower.contains("photographer");
    let illustrator = lower.contains("illustrator");

    CreditsHeadingType::Roles {
        art_director,
        artist,
        designer,
        typographer,
        photographer,
        illustrator,
    }
}

/// Generate the new slug format based on rate_type and rate
/// Format: "{base}-{denomination}-{year}" for denominated, "{base}-{value_type}-{year}" for forever
fn generate_slug(api_slug: &str, year: u32, rate_type: Option<&str>, rate: Option<&str>) -> (String, bool) {
    // Forever stamps only exist from 2007 onwards
    // For pre-2007 stamps, always false
    let is_forever = if year < 2007 {
        false
    } else {
        // Determine if this is a forever stamp based on rate_type
        match rate_type {
            Some("Forever")
            | Some("Semipostal")
            | Some("International")
            | Some("Global Forever")
            | Some("Postcard")
            | Some("Additional Ounce")
            | Some("Additional Postage")
            | Some("Two Ounce")
            | Some("Three Ounce")
            | Some("Nonmachineable Surcharge") => true,
            Some("Presorted Standard") | Some("Presorted First-Class") => false,
            // Without rate_type, assume not forever (denominated)
            None => false,
            _ => false,
        }
    };

    // Clean the API slug to get base name (remove year suffix if present)
    let year_suffix = format!("-{}", year);
    let base_slug = if api_slug.ends_with(&year_suffix) {
        &api_slug[..api_slug.len() - year_suffix.len()]
    } else {
        api_slug
    };

    // Strip disambiguation suffix (-2, -3, etc.)
    let base_slug = if let Some(last_dash) = base_slug.rfind('-') {
        let suffix = &base_slug[last_dash + 1..];
        if suffix.len() == 1
            && suffix
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            &base_slug[..last_dash]
        } else {
            base_slug
        }
    } else {
        base_slug
    };

    // Strip denomination prefix (e.g., "10c-poppies" -> "poppies", "2-floral" -> "floral")
    let base_slug = if let Some(idx) = base_slug.find('-') {
        let prefix = &base_slug[..idx];
        if prefix.ends_with('c')
            && prefix[..prefix.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            // Remove cent prefix like "10c-"
            &base_slug[idx + 1..]
        } else if prefix.chars().all(|c| c.is_ascii_digit()) {
            // Remove dollar prefix like "2-"
            &base_slug[idx + 1..]
        } else {
            base_slug
        }
    } else {
        base_slug
    };

    // Build the new slug
    let slug = if is_forever {
        // Use rate_type for slug, fall back to "forever"
        let value_type = rate_type.unwrap_or("Forever");
        let vt_slug = value_type.to_lowercase().replace(' ', "-");

        // Handle special case: don't duplicate "semipostal" in "alzheimers-semipostal"
        if base_slug.ends_with("-semipostal") && vt_slug == "semipostal" {
            format!("{}-{}", base_slug, year)
        } else {
            format!("{}-{}-{}", base_slug, vt_slug, year)
        }
    } else {
        // Denominated stamp: include denomination in slug
        // Parse rate like "5.00" or "6.70" into slug format like "5d" or "6d70c"
        let denom_slug = rate
            .and_then(|r| {
                let r = r.trim_start_matches('$');
                let parts: Vec<&str> = r.split('.').collect();
                if parts.len() == 2 {
                    let dollars: u32 = parts[0].parse().ok()?;
                    let cents: u32 = parts[1].parse().ok()?;
                    if cents == 0 {
                        Some(format!("{}d", dollars))
                    } else {
                        Some(format!("{}d{:02}c", dollars, cents))
                    }
                } else if parts.len() == 1 {
                    // Just dollars, no decimal
                    let dollars: u32 = parts[0].parse().ok()?;
                    Some(format!("{}d", dollars))
                } else {
                    None
                }
            });

        match denom_slug {
            Some(d) => format!("{}-{}-{}", base_slug, d, year),
            None => format!("{}-{}", base_slug, year),
        }
    };

    (slug, is_forever)
}

fn scrape_stamp(
    client: &CachedClient,
    conn: &Connection,
    api_slug: &str,
    year: u32,
    index: usize,
    total: usize,
    quiet: bool,
    overrides: &HashMap<u32, HashMap<String, StampOverrides>>,
) -> Result<()> {
    let mut stdout = io::stdout();
    let forever_url = format!("https://www.stampsforever.com/stamps/{}", api_slug);

    // Print progress prefix and slug link
    if !quiet {
        print!(
            "[{:02}/{:02}] Scraping: {} Images: [",
            index,
            total,
            osc8_link(&forever_url, api_slug)
        );
        stdout.flush()?;
    }

    // Fetch stamp detail from API
    let api_url = format!("{}/{}", STAMPS_API_URL, api_slug);
    let mut detail: StampDetail = client.fetch_json(&api_url)?;

    // Apply overrides from enrichment/stamps/{year}.conl
    if let Some(year_overrides) = overrides.get(&year) {
        if let Some(stamp_overrides) = year_overrides.get(api_slug) {
            if let Some(ref rt) = stamp_overrides.rate_type {
                detail.rate_type = Some(rt.clone());
            }
            if let Some(ref r) = stamp_overrides.rate {
                detail.rate = Some(r.clone());
            }
            if let Some(ref id) = stamp_overrides.issue_date {
                detail.issue_date = Some(id.clone());
            }
            if let Some(ref il) = stamp_overrides.issue_location {
                detail.issue_location = Some(il.clone());
            }
        }
    }

    // Collect stamp images first (need filename for enrichment lookup)
    let mut stamp_images: Vec<String> = Vec::new();
    let mut sheet_images: Vec<String> = Vec::new();

    // Use api_slug directory structure: data/stamps/{year}/{api_slug}/
    let stamp_dir = PathBuf::from(STAMPS_DIR)
        .join(year.to_string())
        .join(api_slug);
    fs::create_dir_all(&stamp_dir)?;

    for img in &detail.images {
        let clean_url = img.path.split('?').next().unwrap_or(&img.path);
        let img_data = client.fetch_binary(clean_url)?;
        let img_filename = extract_image_filename(clean_url);
        let img_path = stamp_dir.join(&img_filename);
        fs::write(&img_path, &img_data)?;
        if !quiet {
            print!("{}", osc8_link(clean_url, "."));
            stdout.flush()?;
        }
        stamp_images.push(img_filename);
    }

    // Handle stamp_pane (sheet image) separately
    if let Some(pane) = &detail.stamp_pane {
        let clean_url = pane.path.split('?').next().unwrap_or(&pane.path);
        let img_data = client.fetch_binary(clean_url)?;
        let img_filename = extract_image_filename(clean_url);
        let img_path = stamp_dir.join(&img_filename);
        fs::write(&img_path, &img_data)?;
        if !quiet {
            print!("{}", osc8_link(clean_url, "s"));
            stdout.flush()?;
        }
        sheet_images.push(img_filename);
    }

    if !quiet {
        print!("] ");
    }

    // Warn about missing required fields not provided by API or overrides
    let mut missing_fields = Vec::new();
    if detail.rate_type.is_none() {
        missing_fields.push("rate_type");
    }
    if detail.issue_date.is_none() {
        missing_fields.push("issue_date");
    }
    if !missing_fields.is_empty() {
        eprintln!(
            "  WARNING: '{}' ({}) missing: {}",
            api_slug,
            year,
            missing_fields.join(", ")
        );
    }

    // Generate slug based on rate_type and rate
    let (slug, is_forever) = generate_slug(api_slug, year, detail.rate_type.as_deref(), detail.rate.as_deref());

    // Parse credits
    let mut art_director: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut designer: Option<String> = None;
    let mut typographer: Option<String> = None;
    let mut photographer: Option<String> = None;
    let mut illustrator: Option<String> = None;
    let mut embedded_credits: Vec<String> = Vec::new();

    if let Some(groupings) = &detail.people_groupings {
        for grouping in groupings {
            let heading = match &grouping.heading {
                Some(h) => h,
                None => continue,
            };
            match parse_credits_heading(heading) {
                CreditsHeadingType::EmbeddedNames => {
                    let heading_names = parse_credits_names(heading);
                    if !heading_names.is_empty() {
                        embedded_credits.extend(heading_names);
                    } else {
                        for person in &grouping.people {
                            embedded_credits.push(person.name.clone());
                        }
                    }
                }
                CreditsHeadingType::Roles {
                    art_director: has_ad,
                    artist: has_ar,
                    designer: has_de,
                    typographer: has_ty,
                    photographer: has_ph,
                    illustrator: has_il,
                } => {
                    for person in &grouping.people {
                        if has_ad && art_director.is_none() {
                            art_director = Some(person.name.clone());
                        }
                        if has_ar && artist.is_none() {
                            artist = Some(person.name.clone());
                        }
                        if has_de && designer.is_none() {
                            designer = Some(person.name.clone());
                        }
                        if has_ty && typographer.is_none() {
                            typographer = Some(person.name.clone());
                        }
                        if has_ph && photographer.is_none() {
                            photographer = Some(person.name.clone());
                        }
                        if has_il && illustrator.is_none() {
                            illustrator = Some(person.name.clone());
                        }
                    }

                    if grouping.people.is_empty() && heading.to_lowercase().contains(" by ") {
                        if let Some(idx) = heading.to_lowercase().find(" by ") {
                            let name = heading[idx + 4..].trim().to_string();
                            if !name.is_empty() {
                                if has_ad && art_director.is_none() {
                                    art_director = Some(name.clone());
                                }
                                if has_ar && artist.is_none() {
                                    artist = Some(name.clone());
                                }
                                if has_de && designer.is_none() {
                                    designer = Some(name.clone());
                                }
                                if has_ty && typographer.is_none() {
                                    typographer = Some(name.clone());
                                }
                                if has_ph && photographer.is_none() {
                                    photographer = Some(name.clone());
                                }
                                if has_il && illustrator.is_none() {
                                    illustrator = Some(name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Parse issue date and location
    let issue_date = detail
        .issue_date
        .as_ref()
        .and_then(|d| parse_date_to_iso(d));

    let issue_location = detail.issue_location.as_ref().and_then(|loc| {
        let loc = loc.trim();
        if loc.is_empty() || loc == "TBA" {
            None
        } else {
            Some(loc.to_string())
        }
    });

    // Get corrected rate (current rate for forever stamps, API rate for denominated)
    let corrected_rate = get_corrected_rate(
        api_slug,
        detail.rate.as_deref(),
        detail.rate_type.as_deref(),
    );
    let rate: Option<f64> = corrected_rate.as_ref().and_then(|r| r.parse().ok());
    let rate_type = detail.rate_type.as_ref().map(|rt| RateType::from_str(rt));

    // Detect stamp type
    let stamp_type_str = detect_stamp_type(&detail.name);
    let stamp_type = StampType::from_str(stamp_type_str);

    // Build credits struct
    let credits = Credits {
        art_director: art_director.clone(),
        artist: artist.clone(),
        designer: designer.clone(),
        typographer: typographer.clone(),
        photographer: photographer.clone(),
        illustrator: illustrator.clone(),
    };

    // Parse about text
    let about = detail
        .about
        .as_ref()
        .map(|a| html_to_text(a))
        .filter(|t| !t.is_empty())
        .or_else(|| {
            detail
                .caption
                .as_ref()
                .map(|c| html_to_text(c))
                .filter(|t| !t.is_empty())
        });

    // Build initial metadata struct (products added later)
    let mut metadata = StampMetadata {
        name: detail.name.clone(),
        slug: slug.clone(),
        api_slug: api_slug.to_string(),
        url: format!("https://www.stampsforever.com/stamps/{}", api_slug),
        year,
        issue_date,
        issue_location,
        rate,
        rate_type,
        forever: is_forever,
        stamp_type,
        series: detail.series.as_ref().map(|s| s.name.clone()),
        stamp_images: stamp_images.clone(),
        sheet_image: sheet_images.first().cloned(),
        background_color: detail.background_color.clone(),
        credits,
        about,
        products: Vec::new(),
    };

    // Warn if no images
    if stamp_images.is_empty() && sheet_images.is_empty() {
        eprintln!(
            "\nWARNING: No images found for '{}' ({})",
            api_slug, forever_url
        );
    }

    // Process products - download images and insert to DB
    // First, delete existing products for this stamp to handle removed/renamed products
    conn.execute(
        "DELETE FROM products WHERE stamp_slug = ?1",
        rusqlite::params![slug],
    )?;

    if let Some(products) = &detail.product_listings {
        // Filter to included products and deduplicate by cleaned title
        // (removes duplicates like "Coil of 100 (BCA)" and "Coil of 100 (APU)")
        let mut seen_titles = std::collections::HashSet::new();
        let included_products: Vec<&ProductListing> = products
            .iter()
            .filter(|p| is_included_product(&p.product_title))
            .filter(|p| {
                let clean = clean_product_title(&p.product_title);
                seen_titles.insert(clean)
            })
            .collect();

        for product in &included_products {
            let mut image_filenames: Vec<String> = Vec::new();
            if let Some(media) = &product.media {
                for media_item in media {
                    let Some(path) = &media_item.path else {
                        continue;
                    };
                    let clean_url = path.split('?').next().unwrap_or(path);
                    let img_data = client.fetch_binary(clean_url)?;
                    let img_filename = extract_image_filename(clean_url);
                    let img_path = stamp_dir.join(&img_filename);
                    fs::write(&img_path, &img_data)?;
                    if !quiet {
                        print!("{}", osc8_link(clean_url, "p"));
                        stdout.flush()?;
                    }
                    image_filenames.push(img_filename);
                }
            }

            let images_json = if image_filenames.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&image_filenames)?)
            };

            let stamps_forever_url = product
                .product_number
                .as_ref()
                .map(|pn| format!("https://www.stampsforever.com/stamps/{}/{}", api_slug, pn));

            // Clean up product titles (expand abbreviations like PSA, WAG)
            let clean_title = clean_product_title(&product.product_title);
            let clean_long_title = product.long_title.as_ref().map(|t| clean_product_title(t));

            // Parse product metadata from original title (before cleaning)
            let product_metadata = parse_product_metadata(&product.product_title);
            let metadata_json = product_metadata
                .as_ref()
                .map(|m| serde_json::to_string(m).ok())
                .flatten();

            // Add to metadata products
            metadata.products.push(Product {
                title: clean_title.clone(),
                long_title: clean_long_title.clone(),
                price: product.price.clone(),
                postal_store_url: product.postal_store_url.clone(),
                stamps_forever_url: stamps_forever_url.clone(),
                images: image_filenames,
                metadata: product_metadata,
            });

            // Insert into products table
            conn.execute(
                "INSERT OR REPLACE INTO products
                 (stamp_slug, year, title, long_title, price, postal_store_url, stamps_forever_url, images, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    slug,
                    year,
                    clean_title,
                    clean_long_title,
                    product.price,
                    product.postal_store_url,
                    stamps_forever_url,
                    images_json,
                    metadata_json,
                ],
            )?;
        }
    }

    // Serialize metadata to CONL and write
    let conl = serde_conl::to_string(&metadata)?;
    let metadata_path = stamp_dir.join("metadata.conl");
    fs::write(&metadata_path, &conl)?;

    // Build JSON for stamp_images array
    let stamp_images_json = if stamp_images.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&stamp_images)?)
    };

    // Build JSON for credits object
    let mut credits_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    if let Some(ad) = &art_director {
        credits_map.insert(
            "art_director".to_string(),
            serde_json::Value::String(ad.clone()),
        );
    }
    if let Some(ar) = &artist {
        credits_map.insert("artist".to_string(), serde_json::Value::String(ar.clone()));
    }
    if let Some(de) = &designer {
        credits_map.insert(
            "designer".to_string(),
            serde_json::Value::String(de.clone()),
        );
    }
    if let Some(ty) = &typographer {
        credits_map.insert(
            "typographer".to_string(),
            serde_json::Value::String(ty.clone()),
        );
    }
    if let Some(ph) = &photographer {
        credits_map.insert(
            "photographer".to_string(),
            serde_json::Value::String(ph.clone()),
        );
    }
    if let Some(il) = &illustrator {
        credits_map.insert(
            "illustrator".to_string(),
            serde_json::Value::String(il.clone()),
        );
    }
    if !embedded_credits.is_empty() {
        credits_map.insert("sources".to_string(), serde_json::json!(embedded_credits));
    }
    let credits_json = if credits_map.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&credits_map)?)
    };

    // Extract about text
    let about_text: Option<String> = detail
        .about
        .as_ref()
        .map(|a| html_to_text(a))
        .filter(|t| !t.is_empty())
        .or_else(|| {
            detail
                .caption
                .as_ref()
                .map(|c| html_to_text(c))
                .filter(|t| !t.is_empty())
        });

    // Parse ISO date for database
    let iso_date: Option<String> = detail
        .issue_date
        .as_ref()
        .and_then(|d| parse_date_to_iso(d));

    // Insert into stamps table
    conn.execute(
        "INSERT OR REPLACE INTO stamps
         (slug, api_slug, name, url, year, issue_date, issue_location, rate, rate_type, type, series,
          stamp_images, sheet_image, credits, about, background_color, forever)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            slug,
            api_slug,
            detail.name,
            format!("https://www.stampsforever.com/stamps/{}", api_slug),
            year,
            iso_date,
            detail
                .issue_location
                .as_ref()
                .filter(|l| !l.trim().is_empty() && l.trim() != "TBA"),
            corrected_rate,
            detail.rate_type,
            metadata.stamp_type.as_str(),
            detail.series.as_ref().map(|s| &s.name),
            stamp_images_json,
            sheet_images.first(),
            credits_json,
            about_text,
            detail.background_color,
            is_forever as i32,
        ],
    )?;

    if !quiet {
        let dir_name = stamp_dir.file_name().unwrap_or_default().to_string_lossy();
        println!(
            " {} to {}",
            osc8_file_link(&metadata_path.to_string_lossy(), "metadata"),
            osc8_file_link(&stamp_dir.to_string_lossy(), &dir_name)
        );
        stdout.flush()?;
    }
    Ok(())
}

pub fn run_scrape(filter: Option<String>, quiet: bool) -> Result<()> {
    let client = CachedClient::new()?;
    let conn = Connection::open("stamps.db")?;

    // Ensure tables exist
    init_database(&conn)?;

    // Load overrides
    let overrides = load_overrides();

    // Get current year for default range
    let current_year: u32 = 2026;

    // Collect (slug, year) tuples from stampsforever_stamps table
    let stamps: Vec<(String, u32)> = match filter {
        None => {
            // Default: scrape from current_year+1 down to MIN_SCRAPE_YEAR
            let mut all_stamps = Vec::new();
            for year in (MIN_SCRAPE_YEAR..=current_year + 1).rev() {
                let mut stmt = conn.prepare(
                    "SELECT slug, year FROM stampsforever_stamps WHERE year = ?1 ORDER BY issue_date DESC",
                )?;
                let rows = stmt.query_map([year], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })?;
                all_stamps.extend(rows.filter_map(|r| r.ok()));
            }
            all_stamps
        }
        Some(f) => {
            if f.contains(',') {
                let mut all_stamps = Vec::new();
                for year_str in f.split(',') {
                    let year_str = year_str.trim();
                    if year_str.len() == 4 && year_str.chars().all(|c| c.is_ascii_digit()) {
                        let year: u32 = year_str.parse()?;
                        if year < MIN_SCRAPE_YEAR {
                            bail!(
                                "Year {} is before {}. Scraping not supported for years before {}.",
                                year,
                                MIN_SCRAPE_YEAR,
                                MIN_SCRAPE_YEAR
                            );
                        }
                        let mut stmt = conn.prepare(
                            "SELECT slug, year FROM stampsforever_stamps WHERE year = ?1 ORDER BY issue_date DESC",
                        )?;
                        let rows = stmt.query_map([year], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                        })?;
                        all_stamps.extend(rows.filter_map(|r| r.ok()));
                    }
                }
                all_stamps
            } else if f.len() == 4 && f.chars().all(|c| c.is_ascii_digit()) {
                let year: u32 = f.parse()?;
                if year < MIN_SCRAPE_YEAR {
                    bail!(
                        "Year {} is before {}. Scraping not supported for years before {}.",
                        year,
                        MIN_SCRAPE_YEAR,
                        MIN_SCRAPE_YEAR
                    );
                }
                let mut stmt = conn.prepare(
                    "SELECT slug, year FROM stampsforever_stamps WHERE year = ?1 ORDER BY issue_date DESC",
                )?;
                let stamps: Vec<(String, u32)> = stmt
                    .query_map([year], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                stamps
            } else {
                // Single slug
                let mut stmt =
                    conn.prepare("SELECT slug, year FROM stampsforever_stamps WHERE slug = ?1")?;
                let stamps: Vec<(String, u32)> = stmt
                    .query_map([&f], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                stamps
            }
        }
    };

    if stamps.is_empty() {
        bail!("No stamps found matching filter. Run 'stamps sync' first to populate the database.");
    }

    let total = stamps.len();
    if !quiet {
        println!("Scraping {} stamps...\n", total);
    }

    for (i, (slug, year)) in stamps.iter().enumerate() {
        if let Err(e) = scrape_stamp(&client, &conn, slug, *year, i + 1, total, quiet, &overrides) {
            eprintln!("\nError scraping {}: {}", slug, e);
        }
    }

    if !quiet {
        println!("\nDone!");
    }

    Ok(())
}
