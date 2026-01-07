use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::Connection;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

mod generate;

const DOMESTIC_CSV_URL: &str = "https://www.usps.com/business/prices/2025/m-fcm-eddm-retail.csv";
const INTERNATIONAL_HTML_URL: &str = "https://pe.usps.com/text/dmm300/Notice123.htm";
const STAMPS_API_URL: &str = "https://admin.stampsforever.com/api/stamp-issuances";
const CACHE_DIR: &str = "cache";
const STAMPS_DIR: &str = "data/stamps";
const MIN_SCRAPE_YEAR: u32 = 1996;

/// Parse year from date string like "June 17, 2025" or "TBA 2026"
fn parse_year(date_str: &str) -> Option<u32> {
    // Try to find a 4-digit year
    for word in date_str.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit());
        if word.len() == 4 {
            if let Ok(year) = word.parse::<u32>() {
                if year >= 1800 && year <= 2100 {
                    return Some(year);
                }
            }
        }
    }
    None
}

/// Parse date string like "June 17, 2025" to ISO 8601 "2025-06-17"
/// Returns None for TBA dates, panics on invalid date format
fn parse_date_to_iso(date_str: &str) -> Option<String> {
    let date_str = date_str.trim();

    // Skip TBA dates
    if date_str.starts_with("TBA") || date_str.is_empty() {
        return None;
    }

    let months = [
        ("January", "01"),
        ("February", "02"),
        ("March", "03"),
        ("April", "04"),
        ("May", "05"),
        ("June", "06"),
        ("July", "07"),
        ("August", "08"),
        ("September", "09"),
        ("October", "10"),
        ("November", "11"),
        ("December", "12"),
    ];

    // Parse "Month Day, Year" format
    for (month_name, month_num) in &months {
        if date_str.starts_with(month_name) {
            let rest = date_str[month_name.len()..].trim();
            // Parse "Day, Year"
            if let Some((day_str, year_str)) = rest.split_once(',') {
                let day: u32 = day_str
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("Failed to parse day from date: '{}'", date_str));
                let year: u32 = year_str
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("Failed to parse year from date: '{}'", date_str));
                return Some(format!("{:04}-{}-{:02}", year, month_num, day));
            }
        }
    }

    panic!(
        "Failed to parse date: '{}'. Expected format 'Month Day, Year'",
        date_str
    );
}

#[derive(Parser)]
#[command(name = "usps-rates")]
#[command(about = "USPS postage rates and stamp scraper")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch simple USPS postage rates (domestic and international)
    Simple,
    /// Scrape all stamps from stampsforever.com into SQLite
    Stamps {
        #[command(subcommand)]
        action: StampsAction,
    },
}

#[derive(Subcommand)]
enum StampsAction {
    /// Build/update the stamps SQLite database
    Sync {
        /// Output SQLite database file
        #[arg(short, long, default_value = "stamps.db")]
        output: String,
    },
    /// Scrape detailed stamp info, images, and metadata
    ScrapeDetails {
        /// Specific stamp slug or year (e.g., "love-2026" or "2025")
        #[arg(value_name = "SLUG_OR_YEAR")]
        filter: Option<String>,
        /// Quiet mode - suppress progress output
        #[arg(short, long)]
        quiet: bool,
    },
    /// Generate static HTML site in output/ directory
    Generate,
}

#[derive(Debug, Serialize, Deserialize)]
struct PostageRates {
    sources: Sources,
    domestic: DomesticRates,
    international: InternationalRates,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sources {
    domestic_csv: String,
    international_html: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DomesticRates {
    effective_date: String,
    letter: LetterRates,
    postcard: f64,
    additional_ounce: f64,
    nonmachinable_surcharge: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct LetterRates {
    stamped: BTreeMap<String, f64>,
    metered: BTreeMap<String, f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InternationalRates {
    effective_date: String,
    global_forever: f64,
    letter_1oz: f64,
    postcard: f64,
    additional_ounce: f64,
    large_envelope_1oz: f64,
}

fn fetch_url(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; USPSRateScraper/1.0)")
        .build()?;

    let response = client.get(url).send()?;
    let text = response.text()?;
    Ok(text)
}

fn parse_domestic_csv(csv_content: &str) -> Result<DomesticRates> {
    let mut letter_stamped: BTreeMap<String, f64> = BTreeMap::new();
    let mut letter_metered: BTreeMap<String, f64> = BTreeMap::new();
    let mut postcard = 0.0;
    let mut additional_ounce = 0.0;
    let mut nonmachinable_surcharge = 0.0;
    let mut effective_date = String::new();

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(csv_content.as_bytes());

    let mut in_letters_section = false;
    let mut in_metered_section = false;

    for result in reader.records() {
        let record = result?;
        if record.len() == 0 {
            continue;
        }

        let first_col = record.get(0).unwrap_or("").trim();

        // Check for effective date in first row
        if first_col.contains("First-Class Mail and EDDM") {
            if let Some(date_col) = record.get(5) {
                effective_date = date_col.trim().to_string();
            }
        }

        // Track sections
        if first_col == "LETTERS" {
            in_letters_section = true;
            in_metered_section = false;
            continue;
        }
        if first_col == "LETTERS - Metered" {
            in_metered_section = true;
            continue;
        }
        if first_col == "FLATS" || first_col.contains("Additional") || first_col == "Postcard" {
            in_letters_section = false;
            in_metered_section = false;
        }

        // Parse letter rates
        if in_letters_section && !in_metered_section {
            if let Ok(weight) = first_col.parse::<f64>() {
                if let Some(rate_str) = record.get(1) {
                    if let Ok(rate) = rate_str.trim().parse::<f64>() {
                        letter_stamped.insert(format!("{}oz", weight), rate);
                    }
                }
            }
        }

        if in_metered_section {
            if let Ok(weight) = first_col.parse::<f64>() {
                if let Some(rate_str) = record.get(1) {
                    if let Ok(rate) = rate_str.trim().parse::<f64>() {
                        letter_metered.insert(format!("{}oz", weight), rate);
                    }
                }
            }
        }

        // Parse postcard rate
        if first_col == "Postcard" {
            if let Some(rate_str) = record.get(1) {
                if let Ok(rate) = rate_str.trim().parse::<f64>() {
                    postcard = rate;
                }
            }
        }

        // Parse additional ounce rate
        if first_col.contains("Single-Piece Additional Ounce") {
            // The rate is in the last column with a value
            for i in (1..record.len()).rev() {
                if let Some(rate_str) = record.get(i) {
                    if let Ok(rate) = rate_str.trim().parse::<f64>() {
                        additional_ounce = rate;
                        break;
                    }
                }
            }
        }

        // Parse nonmachinable surcharge
        if first_col.contains("Nonmachinable Surcharge") {
            for i in (1..record.len()).rev() {
                if let Some(rate_str) = record.get(i) {
                    if let Ok(rate) = rate_str.trim().parse::<f64>() {
                        nonmachinable_surcharge = rate;
                        break;
                    }
                }
            }
        }
    }

    Ok(DomesticRates {
        effective_date,
        letter: LetterRates {
            stamped: letter_stamped,
            metered: letter_metered,
        },
        postcard,
        additional_ounce,
        nonmachinable_surcharge,
    })
}

fn parse_international_html(html_content: &str) -> Result<InternationalRates> {
    let document = Html::parse_document(html_content);

    // Try to find international rates in the HTML
    // The rates are typically in tables within the document
    let table_selector = Selector::parse("table").unwrap();
    let row_selector = Selector::parse("tr").unwrap();
    let cell_selector = Selector::parse("td, th").unwrap();

    let mut global_forever = 1.70; // Default/fallback value as of July 2025
    let mut letter_1oz = 1.70;
    let mut additional_ounce = 0.29;
    let mut large_envelope_1oz = 3.15;

    // Parse tables looking for international rates
    for table in document.select(&table_selector) {
        let table_text = table.text().collect::<String>();

        // Look for First-Class Mail International tables
        if table_text.contains("International") || table_text.contains("Global") {
            for row in table.select(&row_selector) {
                let cells: Vec<String> = row
                    .select(&cell_selector)
                    .map(|c| c.text().collect::<String>().trim().to_string())
                    .collect();

                if cells.len() >= 2 {
                    let label = cells[0].to_lowercase();

                    // Try to parse rate from second column
                    if let Some(rate_str) = cells.get(1) {
                        let cleaned = rate_str.replace('$', "").replace(',', "");
                        if let Ok(rate) = cleaned.trim().parse::<f64>() {
                            if label.contains("letter") && label.contains("1") {
                                letter_1oz = rate;
                                global_forever = rate;
                            } else if label.contains("additional") {
                                additional_ounce = rate;
                            } else if label.contains("large") || label.contains("flat") {
                                large_envelope_1oz = rate;
                            }
                        }
                    }
                }
            }
        }
    }

    // The international postcard rate equals the 1oz letter rate for Global Forever
    let postcard = global_forever;

    Ok(InternationalRates {
        effective_date: "7/13/2025".to_string(),
        global_forever,
        letter_1oz,
        postcard,
        additional_ounce,
        large_envelope_1oz,
    })
}

// Stamps API response types
#[derive(Debug, Deserialize)]
struct StampsApiResponse {
    data: Vec<StampData>,
    #[allow(dead_code)]
    meta: PaginationMeta,
}

#[derive(Debug, Deserialize)]
struct StampData {
    slug: String,
    name: String,
    issue_date: Option<String>,
    issue_location: Option<String>,
    rate_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct PaginationMeta {
    last_page: u32,
    total: u32,
}

// Detailed stamp API response types
#[derive(Debug, Deserialize)]
struct StampDetail {
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
        // Parse URL and create cache path
        // e.g., https://admin.stampsforever.com/api/stamp-issuances/love-2026
        // -> cache/admin.stampsforever.com/api/stamp-issuances/love-2026
        let url = url.split('?').next().unwrap_or(url); // Strip query params
        if let Some(stripped) = url.strip_prefix("https://") {
            self.cache_dir.join(stripped)
        } else if let Some(stripped) = url.strip_prefix("http://") {
            self.cache_dir.join(stripped)
        } else {
            self.cache_dir.join(url)
        }
    }

    fn fetch_text(&self, url: &str) -> Result<String> {
        let cache_path = self.url_to_cache_path(url);

        // Check cache first
        if cache_path.exists() {
            return fs::read_to_string(&cache_path)
                .with_context(|| format!("Failed to read cache: {:?}", cache_path));
        }

        // Fetch from network
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("Failed to fetch: {}", url))?;

        let text = response
            .text()
            .with_context(|| format!("Failed to read response: {}", url))?;

        // Cache it
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &text)?;

        Ok(text)
    }

    fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let text = self.fetch_text(url)?;
        serde_json::from_str(&text).with_context(|| format!("Failed to parse JSON from: {}", url))
    }

    fn fetch_binary(&self, url: &str) -> Result<Vec<u8>> {
        let cache_path = self.url_to_cache_path(url);

        // Check cache first
        if cache_path.exists() {
            return fs::read(&cache_path)
                .with_context(|| format!("Failed to read cache: {:?}", cache_path));
        }

        // Fetch from network
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("Failed to fetch: {}", url))?;

        let bytes = response
            .bytes()
            .with_context(|| format!("Failed to read response: {}", url))?;

        // Cache it
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &bytes)?;

        Ok(bytes.to_vec())
    }
}

// CONL generation helpers
fn escape_conl_value(s: &str) -> String {
    // Simple escaping - if contains newlines, use multiline format
    s.replace('\\', "\\\\")
}

fn format_multiline_text(text: &str, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);
    // Trim trailing empty lines
    let trimmed = text.trim_end();
    let lines: Vec<&str> = trimmed.lines().collect();
    let mut result = String::from("\"\"\"md\n");
    for line in lines {
        if line.trim().is_empty() {
            result.push('\n');
        } else {
            result.push_str(&indent_str);
            result.push_str(line.trim());
            result.push('\n');
        }
    }
    result
}

fn html_to_text(html: &str) -> String {
    // Simple HTML to text conversion
    let mut text = html.to_string();

    // Convert nbsp to regular space first (before tag processing)
    text = text.replace("&nbsp;", " ");
    text = text.replace("\u{00a0}", " ");

    // Convert emphasis tags to markdown, trimming internal whitespace
    // Handle <strong> and </strong>
    while let Some(start) = text.find("<strong>") {
        if let Some(end) = text[start..].find("</strong>") {
            let end = start + end;
            let inner = &text[start + 8..end];
            let trimmed = inner.trim();
            let before_space = if inner.starts_with(' ') && !text[..start].ends_with(' ') {
                " "
            } else {
                ""
            };
            let after_space = if inner.ends_with(' ') { " " } else { "" };
            text = format!(
                "{}{}**{}**{}{}",
                &text[..start],
                before_space,
                trimmed,
                after_space,
                &text[end + 9..]
            );
        } else {
            break;
        }
    }

    // Handle <em> and </em>
    while let Some(start) = text.find("<em>") {
        if let Some(end) = text[start..].find("</em>") {
            let end = start + end;
            let inner = &text[start + 4..end];
            let trimmed = inner.trim();
            let before_space = if inner.starts_with(' ') && !text[..start].ends_with(' ') {
                " "
            } else {
                ""
            };
            let after_space = if inner.ends_with(' ') { " " } else { "" };
            text = format!(
                "{}{}*{}*{}{}",
                &text[..start],
                before_space,
                trimmed,
                after_space,
                &text[end + 5..]
            );
        } else {
            break;
        }
    }

    // Replace block elements with newlines
    text = text.replace("<br>", "\n");
    text = text.replace("<br/>", "\n");
    text = text.replace("<br />", "\n");
    text = text.replace("</p>", "\n\n");
    text = text.replace("</div>", "\n");

    // Remove all remaining HTML tags
    let document = Html::parse_fragment(&text);
    let result: String = document.root_element().text().collect();

    // Clean up whitespace
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n\n")
}

/// Detect stamp type based on name
/// Returns "card" for stamped cards, "envelope" for stamped envelopes, "stamp" otherwise
fn detect_stamp_type(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("stamped card") || lower.contains("postal card") {
        "card"
    } else if lower.contains("stamped envelope") || lower.contains("postal envelope") {
        "envelope"
    } else {
        "stamp"
    }
}

/// Check if a product should be included (stamps, stationery, purchasable items)
/// Includes: Pane of *, Booklet of *, Coil of *, Stamped Card *, Double Reply *,
///           #* Envelope*, Notecards *, Oversized Postcards *
fn is_included_product(title: &str) -> bool {
    let t = title.to_lowercase();

    // Pane of *
    if t.starts_with("pane of ") {
        return true;
    }

    // Booklet of *, Prestige Booklet of *
    if t.contains("booklet of ") {
        return true;
    }

    // Coil of *
    if t.starts_with("coil of ") {
        return true;
    }

    // Stamped Card *, Double Reply *
    if t.starts_with("stamped card") || t.starts_with("double reply") {
        return true;
    }

    // #* Envelope* (stamped envelopes like "#10 Window Stamped Envelopes")
    if title.starts_with('#') && t.contains("envelope") {
        return true;
    }

    // Notecards *
    if t.starts_with("notecard") {
        return true;
    }

    // Oversized Postcards *
    if t.starts_with("oversized postcard") {
        return true;
    }

    false
}

/// Slug typo fixes - corrects typos in API slugs
const SLUG_TYPO_FIXES: &[(&str, &str)] = &[
    ("columbia-river-george", "columbia-river-gorge"), // Typo: "george" should be "gorge"
];

/// Denomination overrides for stamps where rate_type is null or insufficient
/// Format: (api_slug, denomination_suffix)
/// Use this for stamps where we can't derive the denomination from rate_type
const SLUG_DENOMINATION_OVERRIDES: &[(&str, &str)] = &[
    // Stamps with null rate_type that need explicit denominations
    ("eid", "34c"),       // 2001 first-class rate
    ("eid-2", "forever"), // 2013 Forever stamp
    ("american-flag", "41c"),
];

/// Transform API slug and name with denomination and year suffixes
/// Returns (transformed_slug, transformed_name)
///
/// New slug format: {base-slug}-{denomination}-{year}
/// Examples:
/// - us-flags-2023 → us-flags-forever-2023
/// - apples-2 (Postcard) → apples-postcard-forever-2013
/// - apples (1¢) → apples-1c-2016
/// - statue-of-freedom ($1) → statue-of-freedom-1d-2018
fn transform_slug_and_name(
    name: &str,
    api_slug: &str,
    year: u32,
    rate_type: Option<&str>,
    rate: Option<&str>,
) -> (String, String) {
    let mut slug = api_slug.to_string();
    let transformed_name = name.to_string();

    // Step 1: Apply typo fixes
    for (from, to) in SLUG_TYPO_FIXES {
        if slug == *from {
            slug = to.to_string();
            break;
        }
    }

    // Step 2: Strip year suffix if present (e.g., "us-flags-2023" → "us-flags")
    let year_suffix = format!("-{}", year);
    if slug.ends_with(&year_suffix) {
        slug = slug[..slug.len() - year_suffix.len()].to_string();
    }

    // Step 3: Strip disambiguation suffix (-2, -3, etc.)
    // But only if it's a single digit after the dash
    if let Some(last_dash) = slug.rfind('-') {
        let suffix = &slug[last_dash + 1..];
        if suffix.len() == 1
            && suffix
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            slug = slug[..last_dash].to_string();
        }
    }

    // Step 4: Strip denomination prefix from slug if present (e.g., "2-floral-geometry" → "floral-geometry")
    // Handle dollar prefix in API slug like "2-floral-geometry" for "$2 Floral Geometry"
    if let Some(rest) = name.strip_prefix('$') {
        if let Some(space_idx) = rest.find(' ') {
            let amount = &rest[..space_idx];
            if amount.chars().all(|c| c.is_ascii_digit()) {
                if let Some(slug_rest) = slug.strip_prefix(&format!("{}-", amount)) {
                    slug = slug_rest.to_string();
                }
            }
        }
    }

    // Handle cent prefix in API slug like "10c-poppies" for "10¢ Poppies"
    let mut chars = name.chars().peekable();
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if !digits.is_empty() {
        if let Some(next) = chars.next() {
            if next == '¢' || (next == 'c' && chars.peek() == Some(&' ')) {
                if let Some(slug_rest) = slug.strip_prefix(&format!("{}c-", digits)) {
                    slug = slug_rest.to_string();
                }
            }
        }
    }

    // Step 5: Determine denomination suffix
    let denomination = get_denomination_suffix(api_slug, name, rate_type, rate);

    // Step 6: Reconstruct slug with denomination and year
    if let Some(denom) = denomination {
        slug = format!("{}-{}-{}", slug, denom, year);
    } else {
        // No denomination available - just add year
        slug = format!("{}-{}", slug, year);
    }

    (slug, transformed_name)
}

/// Determine the denomination suffix for a stamp
/// Returns None if denomination cannot be determined
fn get_denomination_suffix(
    api_slug: &str,
    name: &str,
    rate_type: Option<&str>,
    _rate: Option<&str>,
) -> Option<String> {
    // First check hardcoded overrides
    for (override_slug, denom) in SLUG_DENOMINATION_OVERRIDES {
        if api_slug == *override_slug {
            return Some(denom.to_string());
        }
    }

    // Try to extract denomination from name (e.g., "$1 Statue of Freedom" → "1d", "1¢ Apples" → "1c")
    if let Some(denom) = extract_denomination_from_name(name) {
        return Some(denom);
    }

    // Use rate_type to determine suffix
    match rate_type {
        Some("Forever") => Some("forever".to_string()),
        Some("Postcard") => Some("postcard-forever".to_string()),
        Some("International") | Some("Global Forever") => Some("global-forever".to_string()),
        Some("Semipostal") => Some("semipostal".to_string()),
        Some("Additional Ounce") => Some("additional-ounce".to_string()),
        Some("Two Ounce") => Some("2oz".to_string()),
        Some("Three Ounce") => Some("3oz".to_string()),
        Some("Nonmachineable Surcharge") => Some("nonmachinable".to_string()),
        Some("Priority Mail") => Some("priority".to_string()),
        Some("Priority Mail Express") => Some("express".to_string()),
        // For these types, we can't determine a simple suffix
        Some("Other Denomination") | Some("Definitive") | Some("First Class") | Some("Special") => {
            None
        }
        // Skip presorted/nonprofit as they're not consumer stamps
        Some("Presorted First-Class") | Some("Presorted Standard") | Some("Nonprofit") => None,
        Some("Additional Postage") => Some("additional".to_string()),
        _ => None,
    }
}

/// Extract denomination from stamp name
/// "$1 Statue of Freedom" → Some("1d")
/// "1¢ Apples" → Some("1c")
/// "10¢ Poppies" → Some("10c")
fn extract_denomination_from_name(name: &str) -> Option<String> {
    // Check for dollar prefix like "$1 " or "$2 "
    if let Some(rest) = name.strip_prefix('$') {
        if let Some(space_idx) = rest.find(' ') {
            let amount = &rest[..space_idx];
            if amount.chars().all(|c| c.is_ascii_digit()) {
                return Some(format!("{}d", amount));
            }
        }
    }

    // Check for cent prefix like "1¢" or "10c "
    let mut chars = name.chars().peekable();
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if !digits.is_empty() {
        if let Some(next) = chars.next() {
            if next == '¢' || (next == 'c' && chars.peek() == Some(&' ')) {
                return Some(format!("{}c", digits));
            }
        }
    }

    None
}

fn extract_image_filename(url: &str) -> String {
    // https://admin.stampsforever.com/images/abc123.png -> abc123.png
    // Strip ?derivative_type=large etc
    let url = url.split('?').next().unwrap_or(url);
    url.rsplit('/').next().unwrap_or("image.png").to_string()
}

/// Suffixes that should be kept attached to the preceding name
const NAME_SUFFIXES: &[&str] = &["Ph.D.", "M.D.", "Jr.", "Sr.", "II", "III", "IV"];

/// Allowed short names (organizations/acronyms that are valid despite being <10 chars)
const ALLOWED_SHORT_NAMES: &[&str] = &[
    "NASA",
    "ESA",
    "Bob Wick",
    "Tom Bean",
    "Tom Till",
    "QT Luong",
    "Art Wolfe",
    "Kevin Ebi",
];

/// Known source headings (headings that should be treated as source names directly)
const KNOWN_SOURCE_HEADINGS: &[&str] = &["Walt Disney Studios Ink & Paint Department"];

/// Priority Mail Express rate overrides - corrected values for data quality
/// All Priority Mail Express stamps MUST have an entry here or the scraper will panic
const PRIORITY_MAIL_EXPRESS_RATE_OVERRIDES: &[(&str, &str)] = &[
    ("star-cluster", "31.40"),
    ("washington-monument", "12.25"),
    ("space-shuttle-piggyback", "11.75"),
    ("capitol-dome", "13.65"),
    ("x-planes-express", "14.40"),
    ("bixby-creek-bridge", "18.30"),
    ("carmel-mission", "18.95"),
    ("grand-central-terminal", "19.95"),
    ("uss-arizona-memorial", "19.99"),
    ("columbia-river-george", "22.95"), // Note: API has typo "george" instead of "gorge"
    ("gateway-arch", "23.75"),
    ("sleeping-bear-dunes", "24.70"),
    ("bethesda-fountain", "24.70"),
    ("grand-island-ice-caves", "26.35"),
    ("palace-of-fine-arts", "26.95"),
    ("great-smoky-mountains", "28.75"),
    ("cosmic-cliffs", "30.45"),
];

/// Get the corrected rate for a stamp, applying Priority Mail Express overrides
/// Panics if a Priority Mail Express stamp doesn't have a defined override
fn get_corrected_rate(
    slug: &str,
    api_rate: Option<&str>,
    rate_type: Option<&str>,
) -> Option<String> {
    // First, check if this slug has an override (handles all express stamps including
    // great-smoky-mountains which has rate_type "Other Denomination")
    for (override_slug, override_rate) in PRIORITY_MAIL_EXPRESS_RATE_OVERRIDES {
        if *override_slug == slug {
            return Some(override_rate.to_string());
        }
    }

    // If rate_type is "Priority Mail Express" but no override found, panic
    let is_priority_express = rate_type
        .map(|rt| rt == "Priority Mail Express")
        .unwrap_or(false);

    if is_priority_express {
        panic!(
            "Priority Mail Express stamp '{}' does not have a rate override defined!\n\
             Add an entry to PRIORITY_MAIL_EXPRESS_RATE_OVERRIDES in src/main.rs.\n\
             API rate was: {:?}, rate_type: {:?}",
            slug, api_rate, rate_type
        );
    }

    // Not a Priority Mail Express stamp - return API rate as-is
    api_rate.map(|r| r.to_string())
}

fn parse_credits_names(text: &str) -> Vec<String> {
    // "Existing Photos by Fiona M. Donnelly, Matthew Prosser, Martha M. Stewart, and Ross Taylor"
    // -> ["Fiona M. Donnelly", "Matthew Prosser", "Martha M. Stewart", "Ross Taylor"]
    //
    // Also handles: "Edith Widder, Ph.D." -> keeps "Edith Widder, Ph.D." as one name
    // Also handles: "Unknown, 18th c, Cuzco, Peru" -> keeps as single attribution (no " and ")
    //
    // Check for known source headings first - return as single source
    if KNOWN_SOURCE_HEADINGS.contains(&text) {
        return vec![text.to_string()];
    }

    // Extract everything after " by " (case insensitive), or return empty if no names
    let lower = text.to_lowercase();
    let text = if let Some(idx) = lower.find(" by ") {
        text[idx + 4..].to_string()
    } else if lower.ends_with(" by") || lower.starts_with("existing ") {
        // Heading like "Existing Photo by" or "Existing Art" with no embedded name - return empty
        return Vec::new();
    } else {
        // No " by " found, use whole text as-is
        text.to_string()
    };

    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    // If there's no " and " in the text, treat the whole thing as a single attribution
    // This handles cases like "Unknown, 18th c, Cuzco, Peru" which should not be split
    if !text.to_lowercase().contains(" and ") {
        return vec![text.to_string()];
    }

    // First, protect suffixes by replacing ", SUFFIX" with a placeholder
    let mut protected = text.to_string();
    for (i, suffix) in NAME_SUFFIXES.iter().enumerate() {
        protected = protected.replace(&format!(", {}", suffix), &format!("\x00SUFFIX{}\x00", i));
    }

    // Replace ", and " with just ", " for consistent splitting
    let protected = protected.replace(", and ", ", ");

    // Split by ", " and " and "
    let names: Vec<String> = protected
        .split(", ")
        .flat_map(|s| s.split(" and "))
        .map(|s| {
            // Restore suffixes
            let mut name = s.trim().to_string();
            for (i, suffix) in NAME_SUFFIXES.iter().enumerate() {
                name = name.replace(&format!("\x00SUFFIX{}\x00", i), &format!(", {}", suffix));
            }
            name
        })
        .filter(|s| !s.is_empty())
        .collect();

    // Validate - panic if any name is suspiciously short (might indicate a missed suffix)
    for name in &names {
        if name.len() < 9 && !ALLOWED_SHORT_NAMES.contains(&name.as_str()) {
            panic!(
                "Parsed name '{}' is suspiciously short (<10 chars). \
                 This might indicate a missed suffix or should be added to ALLOWED_SHORT_NAMES. \
                 Original text: '{}'",
                name, text
            );
        }
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_credits_names_single() {
        let result = parse_credits_names("Existing Art by Herbert E. Abrams");
        assert_eq!(result, vec!["Herbert E. Abrams"]);
    }

    #[test]
    fn test_parse_credits_names_multiple_with_oxford_comma() {
        let result = parse_credits_names(
            "Existing Photography by Steven Haddock, Taylor F. Lockwood, Gail Shumway, \
             Edith Widder, Ph.D., Gregory G. Dimijian, and Danté Fenolio",
        );
        assert_eq!(
            result,
            vec![
                "Steven Haddock",
                "Taylor F. Lockwood",
                "Gail Shumway",
                "Edith Widder, Ph.D.",
                "Gregory G. Dimijian",
                "Danté Fenolio"
            ]
        );
    }

    #[test]
    fn test_parse_credits_names_simple_and() {
        let result = parse_credits_names("Existing Photos by John Smith and Mary Johnson");
        assert_eq!(result, vec!["John Smith", "Mary Johnson"]);
    }

    #[test]
    fn test_parse_credits_names_photos_by() {
        let result = parse_credits_names(
            "Existing Photos by Fiona M. Donnelly, Matthew Prosser, Martha M. Stewart, and Ross Taylor"
        );
        assert_eq!(
            result,
            vec![
                "Fiona M. Donnelly",
                "Matthew Prosser",
                "Martha M. Stewart",
                "Ross Taylor"
            ]
        );
    }

    #[test]
    fn test_get_corrected_rate_priority_mail_express() {
        // Should return override rate for Priority Mail Express stamps
        let rate = get_corrected_rate("star-cluster", Some("99.99"), Some("Priority Mail Express"));
        assert_eq!(rate, Some("31.40".to_string()));
    }

    #[test]
    fn test_get_corrected_rate_great_smoky_mountains() {
        // great-smoky-mountains has "Other Denomination" but should still use override
        let rate = get_corrected_rate(
            "great-smoky-mountains",
            Some("99.99"),
            Some("Other Denomination"),
        );
        assert_eq!(rate, Some("28.75".to_string()));
    }

    #[test]
    fn test_get_corrected_rate_regular_stamp() {
        // Regular stamps should use API rate as-is
        let rate = get_corrected_rate("love-2026", Some("0.73"), Some("Forever"));
        assert_eq!(rate, Some("0.73".to_string()));
    }

    #[test]
    fn test_get_corrected_rate_other_denomination_not_express() {
        // Other Denomination stamps not in override list should use API rate
        let rate = get_corrected_rate(
            "10c-poppies-and-coneflowers",
            Some("0.10"),
            Some("Other Denomination"),
        );
        assert_eq!(rate, Some("0.10".to_string()));
    }

    #[test]
    #[should_panic(expected = "does not have a rate override defined")]
    fn test_get_corrected_rate_unknown_priority_mail_express_panics() {
        // Unknown Priority Mail Express stamp should panic
        get_corrected_rate(
            "unknown-express-stamp",
            Some("50.00"),
            Some("Priority Mail Express"),
        );
    }

    // Tests for slug transformation

    #[test]
    fn test_transform_slug_forever_with_year() {
        // us-flags-2023 → us-flags-forever-2023
        let (slug, _name) = transform_slug_and_name(
            "U.S. Flag",
            "us-flags-2023",
            2023,
            Some("Forever"),
            Some("0.78"),
        );
        assert_eq!(slug, "us-flags-forever-2023");
    }

    #[test]
    fn test_transform_slug_postcard() {
        // coastal-birds → coastal-birds-postcard-forever-2015
        let (slug, _name) = transform_slug_and_name(
            "Coastal Birds",
            "coastal-birds",
            2015,
            Some("Postcard"),
            Some("0.61"),
        );
        assert_eq!(slug, "coastal-birds-postcard-forever-2015");
    }

    #[test]
    fn test_transform_slug_disambiguation_suffix_removed() {
        // apples-2 (Postcard) → apples-postcard-forever-2013
        let (slug, _name) =
            transform_slug_and_name("Apples", "apples-2", 2013, Some("Postcard"), Some("0.61"));
        assert_eq!(slug, "apples-postcard-forever-2013");
    }

    #[test]
    fn test_transform_slug_cent_denomination() {
        // apples (1¢) → apples-1c-2016
        let (slug, _name) = transform_slug_and_name(
            "1¢ Apples",
            "apples",
            2016,
            Some("Other Denomination"),
            None,
        );
        assert_eq!(slug, "apples-1c-2016");
    }

    #[test]
    fn test_transform_slug_dollar_denomination() {
        // statue-of-freedom ($1) → statue-of-freedom-1d-2018
        let (slug, _name) = transform_slug_and_name(
            "$1 Statue of Freedom",
            "statue-of-freedom",
            2018,
            Some("Definitive"),
            None,
        );
        assert_eq!(slug, "statue-of-freedom-1d-2018");
    }

    #[test]
    fn test_transform_slug_international() {
        // poinsettia (International) → poinsettia-global-forever-2018
        let (slug, _name) = transform_slug_and_name(
            "Poinsettia",
            "poinsettia",
            2018,
            Some("International"),
            Some("1.70"),
        );
        assert_eq!(slug, "poinsettia-global-forever-2018");
    }

    #[test]
    fn test_transform_slug_typo_fix() {
        // columbia-river-george → columbia-river-gorge-express-YEAR
        let (slug, _name) = transform_slug_and_name(
            "Columbia River Gorge",
            "columbia-river-george",
            2019,
            Some("Priority Mail Express"),
            Some("22.95"),
        );
        assert_eq!(slug, "columbia-river-gorge-express-2019");
    }

    #[test]
    fn test_transform_slug_denomination_override() {
        // eid (null rate_type, but has override) → eid-34c-2001
        let (slug, _name) = transform_slug_and_name("Eid", "eid", 2001, None, None);
        assert_eq!(slug, "eid-34c-2001");
    }

    #[test]
    fn test_transform_slug_denomination_override_forever() {
        // eid-2 (null rate_type, but has override for forever) → eid-forever-2013
        let (slug, _name) = transform_slug_and_name("Eid", "eid-2", 2013, None, None);
        assert_eq!(slug, "eid-forever-2013");
    }

    #[test]
    fn test_transform_slug_hanukkah_forever() {
        // hanukkah-2016 → hanukkah-forever-2016
        let (slug, _name) = transform_slug_and_name(
            "Hanukkah",
            "hanukkah-2016",
            2016,
            Some("Forever"),
            Some("0.78"),
        );
        assert_eq!(slug, "hanukkah-forever-2016");
    }
}

/// Represents the type of credits heading
enum CreditsHeadingType {
    /// Contains names embedded in the heading (e.g., "Existing Photo by John Smith")
    EmbeddedNames,
    /// Contains people in the people array with specific roles
    Roles {
        art_director: bool,
        artist: bool,
        designer: bool,
        typographer: bool,
        photographer: bool,
        illustrator: bool,
    },
}

fn parse_credits_heading(heading: &str) -> CreditsHeadingType {
    let h = heading.to_lowercase();

    // Check for known source headings (exact match)
    if KNOWN_SOURCE_HEADINGS.contains(&heading) {
        return CreditsHeadingType::EmbeddedNames;
    }

    // Check for "Existing X" patterns - names may be in heading or people array
    // Covers: "Existing Art by John", "Existing Photo by", "Existing Art" (no "by" at all)
    if h.starts_with("existing ") {
        return CreditsHeadingType::EmbeddedNames;
    }

    // Check for specific photo/art/illustration credit patterns without "existing"
    // e.g., "Illustrated by Ted Rose", "Art by John Smith"
    if (h.contains("photo") || h.contains("art by") || h.contains("illustrated by"))
        && h.contains(" by ")
    {
        return CreditsHeadingType::EmbeddedNames;
    }

    let has_art_director = h.contains("art director");
    let has_artist = h.contains("artist");
    let has_designer = h.contains("designer") || h.contains("design");
    let has_typographer = h.contains("typographer") || h.contains("typography");
    let has_photographer = h.contains("photographer");
    let has_illustrator = h.contains("illustrator");

    // Panic if we encounter an unknown heading type
    if !has_art_director
        && !has_artist
        && !has_designer
        && !has_typographer
        && !has_photographer
        && !has_illustrator
    {
        panic!(
            "Unknown people_groupings heading: '{}'. \
             Expected 'art director', 'artist', 'designer', 'typographer', 'photographer', 'illustrator', or 'Existing X by'.",
            heading
        );
    }

    CreditsHeadingType::Roles {
        art_director: has_art_director,
        artist: has_artist,
        designer: has_designer,
        typographer: has_typographer,
        photographer: has_photographer,
        illustrator: has_illustrator,
    }
}

// OSC8 hyperlink helpers
fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

fn osc8_file_link(path: &str, text: &str) -> String {
    let abs_path = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());
    format!("\x1b]8;;file://{}\x1b\\{}\x1b]8;;\x1b\\", abs_path, text)
}

fn run_simple() -> Result<()> {
    println!("Fetching USPS domestic rates...");
    let domestic_csv = fetch_url(DOMESTIC_CSV_URL).context("Failed to fetch domestic CSV")?;

    println!("Fetching USPS international rates...");
    let international_html =
        fetch_url(INTERNATIONAL_HTML_URL).context("Failed to fetch international HTML")?;

    println!("Parsing domestic rates...");
    let domestic = parse_domestic_csv(&domestic_csv).context("Failed to parse domestic CSV")?;

    println!("Parsing international rates...");
    let international = parse_international_html(&international_html)
        .context("Failed to parse international HTML")?;

    let rates = PostageRates {
        sources: Sources {
            domestic_csv: DOMESTIC_CSV_URL.to_string(),
            international_html: INTERNATIONAL_HTML_URL.to_string(),
        },
        domestic,
        international,
    };

    let json = serde_json::to_string_pretty(&rates)?;

    // Write to file
    fs::write("rates.json", &json)?;
    println!("Rates written to rates.json");

    // Also print to stdout
    println!("\n{}", json);

    Ok(())
}

fn init_database(conn: &Connection) -> Result<()> {
    // Create stamps table (basic info from API listing)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS stamps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            rate TEXT,
            year INTEGER,
            issue_date TEXT,
            issue_location TEXT,
            forever_url TEXT NOT NULL,
            forever_slug TEXT NOT NULL UNIQUE,
            type TEXT NOT NULL DEFAULT 'stamp'
        )",
        [],
    )?;

    // Create stamp_metadata table (detailed info from scraping)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS stamp_metadata (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            year INTEGER NOT NULL,
            issue_date TEXT,
            issue_location TEXT,
            rate TEXT,
            rate_type TEXT,
            type TEXT NOT NULL DEFAULT 'stamp',
            series TEXT,
            stamp_images JSONB,
            sheet_image TEXT,
            credits JSONB,
            about TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        )",
        [],
    )?;

    // Create index for year lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_stamp_metadata_year ON stamp_metadata(year)",
        [],
    )?;

    // Create products table (purchasable items from stamp pages)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS products (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            stamp_slug TEXT NOT NULL,
            year INTEGER NOT NULL,
            title TEXT NOT NULL,
            long_title TEXT,
            price TEXT,
            postal_store_url TEXT,
            stamps_forever_url TEXT,
            images JSONB,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now')),
            UNIQUE(stamp_slug, title)
        )",
        [],
    )?;

    // Create index for stamp_slug lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_products_stamp_slug ON products(stamp_slug)",
        [],
    )?;

    Ok(())
}

fn run_stamps(output: &str) -> Result<()> {
    // Create/open SQLite database
    let conn = Connection::open(output)?;

    init_database(&conn)?;

    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; USPSStampScraper/1.0)")
        .build()?;

    // Fetch all stamps in a single request (API supports up to 5000 per page)
    let url = format!("{}?per_page=5000", STAMPS_API_URL);

    println!("Fetching stamps from API...");
    let response: StampsApiResponse = client
        .get(&url)
        .send()
        .context("Failed to fetch stamps API")?
        .json()
        .context("Failed to parse stamps JSON")?;

    let mut total_inserted = 0u32;

    for stamp in &response.data {
        let forever_url = format!("https://www.stampsforever.com/stamps/{}", stamp.slug);

        // Parse year from issue_date (works for "June 17, 2025" and "TBA 2026")
        let year: Option<u32> = stamp.issue_date.as_ref().and_then(|d| parse_year(d));

        // Parse issue_date to ISO 8601, None for TBA dates
        let iso_date: Option<String> = stamp.issue_date.as_ref().and_then(|d| parse_date_to_iso(d));

        // Detect stamp type (stamp, card, envelope)
        let stamp_type = detect_stamp_type(&stamp.name);

        let result = conn.execute(
            "INSERT OR REPLACE INTO stamps (name, rate, year, issue_date, issue_location, forever_url, forever_slug, type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                &stamp.name,
                &stamp.rate_type,
                &year,
                &iso_date,
                &stamp.issue_location,
                &forever_url,
                &stamp.slug,
                stamp_type,
            ),
        );

        match result {
            Ok(_) => total_inserted += 1,
            Err(e) => eprintln!("  Error inserting {}: {}", stamp.name, e),
        }
    }

    println!("Done! Inserted {} stamps into {}", total_inserted, output);
    Ok(())
}

fn scrape_stamp_details(
    client: &CachedClient,
    conn: &Connection,
    slug: &str,
    year: u32,
    index: usize,
    total: usize,
    quiet: bool,
) -> Result<()> {
    let mut stdout = io::stdout();
    let forever_url = format!("https://www.stampsforever.com/stamps/{}", slug);

    // Print progress prefix and slug link
    if !quiet {
        print!(
            "[{:02}/{:02}] Scraping: {} Images: [",
            index,
            total,
            osc8_link(&forever_url, slug)
        );
        stdout.flush()?;
    }

    // Fetch stamp detail from API
    let api_url = format!("{}/{}", STAMPS_API_URL, slug);
    let detail: StampDetail = client.fetch_json(&api_url)?;

    // Transform slug and name (adds denomination and year suffixes)
    let (transformed_slug, transformed_name) = transform_slug_and_name(
        &detail.name,
        &detail.slug,
        year,
        detail.rate_type.as_deref(),
        detail.rate.as_deref(),
    );
    let stamp_dir = PathBuf::from(STAMPS_DIR)
        .join(year.to_string())
        .join(&transformed_slug);
    fs::create_dir_all(&stamp_dir)?;

    // Collect stamp images
    let mut stamp_images: Vec<String> = Vec::new();
    let mut sheet_images: Vec<String> = Vec::new();

    for img in &detail.images {
        // Download image (strip query params)
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

    // Parse credits
    let mut art_director: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut designer: Option<String> = None;
    let mut typographer: Option<String> = None;
    let mut photographer: Option<String> = None;
    let mut illustrator: Option<String> = None;
    let mut embedded_credits: Vec<String> = Vec::new(); // Photo/art credits from heading

    if let Some(groupings) = &detail.people_groupings {
        for grouping in groupings {
            // Skip groupings with null heading
            let heading = match &grouping.heading {
                Some(h) => h,
                None => continue,
            };
            match parse_credits_heading(heading) {
                CreditsHeadingType::EmbeddedNames => {
                    // Parse names embedded in heading (e.g., "Existing Photo by John Smith")
                    let heading_names = parse_credits_names(heading);
                    if !heading_names.is_empty() {
                        embedded_credits.extend(heading_names);
                    } else {
                        // No names in heading, use people array instead
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

                    // If people array is empty but heading contains " by ", extract name from heading
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

    // Generate CONL metadata
    let mut conl = String::new();
    conl.push_str(&format!(
        "name = {}\n",
        escape_conl_value(&transformed_name)
    ));
    conl.push_str(&format!("slug = {}\n", transformed_slug));
    conl.push_str(&format!(
        "url = https://www.stampsforever.com/stamps/{}\n",
        detail.slug // Keep original slug for URL
    ));

    // Issue date (ISO 8601 format, skip if TBA)
    if let Some(date) = &detail.issue_date {
        if let Some(iso_date) = parse_date_to_iso(date) {
            conl.push_str(&format!("issue_date = {}\n", iso_date));
        }
    }

    // Issue location (skip if TBA or empty)
    if let Some(loc) = &detail.issue_location {
        let loc = loc.trim();
        if !loc.is_empty() && loc != "TBA" {
            conl.push_str(&format!("issue_location = {}\n", loc));
        }
    }

    // Rate (numeric value, e.g., "0.78") - apply Priority Mail Express overrides
    let corrected_rate = get_corrected_rate(
        &detail.slug,
        detail.rate.as_deref(),
        detail.rate_type.as_deref(),
    );
    if let Some(rate) = &corrected_rate {
        conl.push_str(&format!("rate = {}\n", rate));
    }

    // Rate type (category, e.g., "Forever")
    if let Some(rate_type) = &detail.rate_type {
        conl.push_str(&format!("rate_type = {}\n", rate_type));
    }

    // Type (only include if not "stamp" since that's the default)
    let stamp_type = detect_stamp_type(&detail.name);
    if stamp_type != "stamp" {
        conl.push_str(&format!("type = {}\n", stamp_type));
    }

    // Series
    if let Some(series) = &detail.series {
        conl.push_str(&format!("series = {}\n", series.name));
    }

    // Year
    conl.push_str(&format!("year = {}\n", year));

    // Stamp images (array at top level)
    if stamp_images.is_empty() && sheet_images.is_empty() {
        eprintln!(
            "\nWARNING: No images found for '{}' ({})",
            slug, forever_url
        );
    } else {
        if !stamp_images.is_empty() {
            conl.push_str("stamp_images\n");
            for img in &stamp_images {
                conl.push_str(&format!("  = {}\n", img));
            }
        }
        // Sheet image (single value at top level)
        if !sheet_images.is_empty() {
            // Only use first sheet image since it's a single field now
            conl.push_str(&format!("sheet_image = {}\n", sheet_images[0]));
        }
    }

    // Credits
    if art_director.is_some()
        || artist.is_some()
        || designer.is_some()
        || typographer.is_some()
        || photographer.is_some()
        || illustrator.is_some()
        || !embedded_credits.is_empty()
    {
        conl.push_str("credits\n");
        if let Some(ad) = &art_director {
            conl.push_str(&format!("  art_director = {}\n", ad));
        }
        if let Some(ar) = &artist {
            conl.push_str(&format!("  artist = {}\n", ar));
        }
        if let Some(de) = &designer {
            conl.push_str(&format!("  designer = {}\n", de));
        }
        if let Some(ty) = &typographer {
            conl.push_str(&format!("  typographer = {}\n", ty));
        }
        if let Some(ph) = &photographer {
            conl.push_str(&format!("  photographer = {}\n", ph));
        }
        if let Some(il) = &illustrator {
            conl.push_str(&format!("  illustrator = {}\n", il));
        }
        if !embedded_credits.is_empty() {
            conl.push_str("  sources\n");
            for name in &embedded_credits {
                conl.push_str(&format!("    = {}\n", name));
            }
        }
    }

    // About/description
    if let Some(about) = &detail.about {
        let about_text = html_to_text(about);
        if !about_text.is_empty() {
            conl.push_str(&format!(
                "about = {}\n",
                format_multiline_text(&about_text, 1)
            ));
        }
    } else if let Some(caption) = &detail.caption {
        let caption_text = html_to_text(caption);
        if !caption_text.is_empty() {
            conl.push_str(&format!(
                "about = {}\n",
                format_multiline_text(&caption_text, 1)
            ));
        }
    }

    // Process products - download images to stamp_dir and build entries
    // Tuple: (title, long_title, price, postal_store_url, stamps_forever_url, images)
    let mut products_conl_entries: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<String>,
    )> = Vec::new();

    if let Some(products) = &detail.product_listings {
        // Filter to only included products with postal_store_url
        let included_products: Vec<&ProductListing> = products
            .iter()
            .filter(|p| is_included_product(&p.product_title) && p.postal_store_url.is_some())
            .collect();

        for product in &included_products {
            // Download all product images to stamp_dir (same location as stamp images)
            let mut image_filenames: Vec<String> = Vec::new();
            if let Some(media) = &product.media {
                for media_item in media {
                    // Skip video items (they have url instead of path)
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

            // Build JSON for images array (for database)
            let images_json = if image_filenames.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&image_filenames)?)
            };

            // Construct stamps_forever_url from slug and product_number
            let stamps_forever_url = product.product_number.as_ref().map(|pn| {
                format!(
                    "https://www.stampsforever.com/stamps/{}/{}",
                    detail.slug, pn
                )
            });

            // Store for CONL generation
            products_conl_entries.push((
                product.product_title.clone(),
                product.long_title.clone(),
                product.price.clone(),
                product.postal_store_url.clone(),
                stamps_forever_url.clone(),
                image_filenames,
            ));

            // Insert into products table
            conn.execute(
                "INSERT OR REPLACE INTO products
                 (stamp_slug, year, title, long_title, price, postal_store_url, stamps_forever_url, images, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))",
                rusqlite::params![
                    transformed_slug,
                    year,
                    product.product_title,
                    product.long_title,
                    product.price,
                    product.postal_store_url,
                    stamps_forever_url,
                    images_json,
                ],
            )?;
        }
    }

    // Add products section to CONL (list of maps format)
    if !products_conl_entries.is_empty() {
        conl.push_str("products\n");
        for (title, long_title, price, postal_store_url, stamps_forever_url, images) in
            &products_conl_entries
        {
            conl.push_str("  =\n");
            conl.push_str(&format!("    title = {}\n", title));
            if let Some(lt) = long_title {
                conl.push_str(&format!("    long_title = {}\n", lt));
            }
            if let Some(p) = price {
                conl.push_str(&format!("    price = {}\n", p));
            }
            if let Some(url) = postal_store_url {
                conl.push_str(&format!("    postal_store_url = {}\n", url));
            }
            if let Some(url) = stamps_forever_url {
                conl.push_str(&format!("    stamps_forever_url = {}\n", url));
            }
            if !images.is_empty() {
                conl.push_str("    images\n");
                for img in images {
                    conl.push_str(&format!("      = {}\n", img));
                }
            }
        }
    }

    // Write metadata.conl
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

    // Insert into stamp_metadata table (use corrected_rate instead of detail.rate)
    conn.execute(
        "INSERT OR REPLACE INTO stamp_metadata
         (slug, name, url, year, issue_date, issue_location, rate, rate_type, type, series,
          stamp_images, sheet_image, credits, about, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, datetime('now'))",
        rusqlite::params![
            transformed_slug,
            transformed_name,
            format!("https://www.stampsforever.com/stamps/{}", detail.slug),
            year,
            iso_date,
            detail
                .issue_location
                .as_ref()
                .filter(|l| !l.trim().is_empty() && l.trim() != "TBA"),
            corrected_rate,
            detail.rate_type,
            stamp_type,
            detail.series.as_ref().map(|s| &s.name),
            stamp_images_json,
            sheet_images.first(),
            credits_json,
            about_text,
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

fn run_scrape_details(filter: Option<String>, quiet: bool) -> Result<()> {
    let client = CachedClient::new()?;
    let conn = Connection::open("stamps.db")?;

    // Ensure metadata table exists
    init_database(&conn)?;

    // Get current year for default range
    let current_year: u32 = 2026; // TODO: could use chrono but keeping it simple

    // Collect (slug, year) tuples
    let stamps: Vec<(String, u32)> = match filter {
        None => {
            // Default: scrape from current_year+1 down to MIN_SCRAPE_YEAR
            let mut all_stamps = Vec::new();
            for year in (MIN_SCRAPE_YEAR..=current_year + 1).rev() {
                let mut stmt = conn.prepare(
                    "SELECT forever_slug, year FROM stamps WHERE year = ?1 ORDER BY issue_date DESC",
                )?;
                let rows = stmt.query_map([year], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })?;
                all_stamps.extend(rows.filter_map(|r| r.ok()));
            }
            all_stamps
        }
        Some(f) => {
            // Check if it contains comma (multiple years)
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
                            "SELECT forever_slug, year FROM stamps WHERE year = ?1 ORDER BY issue_date DESC",
                        )?;
                        let rows = stmt.query_map([year], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                        })?;
                        all_stamps.extend(rows.filter_map(|r| r.ok()));
                    } else {
                        bail!("Invalid year in list: '{}'", year_str);
                    }
                }
                all_stamps
            } else if f.len() == 4 && f.chars().all(|c| c.is_ascii_digit()) {
                // Single year
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
                    "SELECT forever_slug, year FROM stamps WHERE year = ?1 ORDER BY issue_date DESC",
                )?;
                let rows = stmt.query_map([year], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })?;
                rows.filter_map(|r| r.ok()).collect()
            } else {
                // Single slug - look up year from database
                let mut stmt =
                    conn.prepare("SELECT forever_slug, year FROM stamps WHERE forever_slug = ?1")?;
                let rows = stmt.query_map([&f], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })?;
                rows.filter_map(|r| r.ok()).collect()
            }
        }
    };

    if stamps.is_empty() {
        bail!("No stamps found matching filter");
    }

    let total = stamps.len();
    if !quiet {
        println!("Scraping {} stamps...", total);
    }

    for (i, (slug, year)) in stamps.iter().enumerate() {
        if let Err(e) = scrape_stamp_details(&client, &conn, slug, *year, i + 1, total, quiet) {
            eprintln!("\nError scraping {}: {}", slug, e);
            // Continue with next stamp instead of failing completely
        }
    }

    if !quiet {
        println!("Done!");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Simple => run_simple(),
        Commands::Stamps { action } => match action {
            StampsAction::Sync { output } => run_stamps(&output),
            StampsAction::ScrapeDetails { filter, quiet } => run_scrape_details(filter, quiet),
            StampsAction::Generate => generate::run_generate(),
        },
    }
}
