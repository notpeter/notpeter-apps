use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const OUTPUT_DIR: &str = "output";
const DATA_DIR: &str = "data/stamps";
const MIN_YEAR: u32 = 2007;

// Rate types to hide
const HIDDEN_RATE_TYPES: &[&str] = &[
    "Federal Duck Stamp",
    "Presorted Standard",
    "Presorted First-Class",
    "Nonprofit",
];

/// Parsed stamp metadata from CONL file
#[derive(Debug, Clone)]
pub struct Stamp {
    pub name: String,
    pub slug: String,
    pub url: String,
    pub year: u32,
    pub issue_date: Option<String>,
    pub issue_location: Option<String>,
    pub rate: Option<f64>,
    pub rate_type: Option<String>,
    pub stamp_type: String, // "stamp", "card", "envelope"
    pub series: Option<String>,
    pub stamp_images: Vec<String>,
    pub sheet_image: Option<String>,
    pub credits: Credits,
    pub about: Option<String>,
    pub products: Vec<Product>,
}

#[derive(Debug, Clone, Default)]
pub struct Credits {
    pub art_director: Option<String>,
    pub artist: Option<String>,
    pub designer: Option<String>,
    pub typographer: Option<String>,
    pub photographer: Option<String>,
    pub illustrator: Option<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Product {
    pub title: String,
    pub long_title: Option<String>,
    pub price: Option<String>,
    pub postal_store_url: Option<String>,
    pub stamps_forever_url: Option<String>,
    pub images: Vec<String>,
}

/// Denomination category for grouping stamps
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DenominationCategory {
    Forever,
    ForeverPlus, // Additional ounce, 2oz, 3oz, non-machinable
    Postcard,
    Global,
    Denominated(String), // Specific cent/dollar value
    Priority,
    Express,
    Other,
}

impl DenominationCategory {
    fn from_stamp(stamp: &Stamp) -> Self {
        let rate_type = stamp.rate_type.as_deref().unwrap_or("");

        match rate_type {
            "Forever" => DenominationCategory::Forever,
            "Postcard" => DenominationCategory::Postcard,
            "International" | "Global Forever" => DenominationCategory::Global,
            "Additional Ounce" | "Two Ounce" | "Three Ounce" | "Nonmachineable Surcharge" | "Additional Postage" => {
                DenominationCategory::ForeverPlus
            }
            "Priority Mail" => DenominationCategory::Priority,
            "Priority Mail Express" => DenominationCategory::Express,
            "Definitive" | "Other Denomination" | "First Class" | "Special" => {
                // Check if it has a specific denomination in the name
                if let Some(denom) = extract_denomination(&stamp.name) {
                    DenominationCategory::Denominated(denom)
                } else if let Some(rate) = stamp.rate {
                    DenominationCategory::Denominated(format_rate(rate))
                } else {
                    DenominationCategory::Other
                }
            }
            "Semipostal" => DenominationCategory::Forever, // Group with Forever
            _ => {
                if let Some(denom) = extract_denomination(&stamp.name) {
                    DenominationCategory::Denominated(denom)
                } else {
                    DenominationCategory::Other
                }
            }
        }
    }

    fn display_name(&self) -> &str {
        match self {
            DenominationCategory::Forever => "Forever Stamps",
            DenominationCategory::ForeverPlus => "Additional Postage Forever Stamps",
            DenominationCategory::Postcard => "Postcard Forever Stamps",
            DenominationCategory::Global => "Global Forever Stamps",
            DenominationCategory::Denominated(_) => "Denominated Stamps",
            DenominationCategory::Priority => "Priority Mail Stamps",
            DenominationCategory::Express => "Priority Mail Express Stamps",
            DenominationCategory::Other => "Other Stamps",
        }
    }
}

/// Extract denomination from stamp name (e.g., "1¢ Apples" -> "1c", "$1 Liberty" -> "$1")
fn extract_denomination(name: &str) -> Option<String> {
    // Check for dollar prefix
    if name.starts_with('$') {
        if let Some(space_idx) = name.find(' ') {
            let amount = &name[1..space_idx];
            if amount.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return Some(format!("${}", amount));
            }
        }
    }

    // Check for cent prefix
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
                return Some(format!("{}¢", digits));
            }
        }
    }

    None
}

/// Format rate as display string
fn format_rate(rate: f64) -> String {
    if rate >= 1.0 {
        format!("${:.2}", rate)
    } else {
        format!("{}¢", (rate * 100.0).round() as u32)
    }
}

/// Simple CONL parser
fn parse_conl(content: &str) -> Result<BTreeMap<String, ConlValue>> {
    let mut result = BTreeMap::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Check for key = value
        if let Some((key, value)) = trimmed.split_once(" = ") {
            let key = key.trim();
            let value = value.trim();

            // Check for multiline string
            if value.starts_with("\"\"\"") {
                let mut multiline = String::new();
                i += 1;
                while i < lines.len() {
                    let ml_line = lines[i];
                    // End when we hit a line that's not indented or is a new key
                    if !ml_line.starts_with("  ") && !ml_line.trim().is_empty() {
                        break;
                    }
                    if !multiline.is_empty() {
                        multiline.push('\n');
                    }
                    multiline.push_str(ml_line.trim());
                    i += 1;
                }
                result.insert(key.to_string(), ConlValue::String(multiline));
                continue;
            }

            result.insert(key.to_string(), ConlValue::String(value.to_string()));
            i += 1;
        }
        // Check for nested object or array (key on its own line)
        else if !trimmed.contains(" = ") && !trimmed.starts_with("=") {
            let key = trimmed;
            i += 1;

            // Look at next lines to determine if it's an array or object
            let mut is_array = false;
            let mut is_object_array = false;

            if i < lines.len() {
                let next_line = lines[i].trim();
                if next_line.starts_with("= ") || next_line == "=" {
                    is_array = true;
                    if next_line == "=" {
                        is_object_array = true;
                    }
                }
            }

            if is_object_array {
                // Array of objects (products)
                let mut objects = Vec::new();
                while i < lines.len() {
                    let check_line = lines[i];
                    if !check_line.starts_with("  ") && !check_line.trim().is_empty() {
                        break;
                    }
                    let trimmed_check = check_line.trim();
                    if trimmed_check == "=" {
                        // Start new object
                        let mut obj = BTreeMap::new();
                        i += 1;
                        while i < lines.len() {
                            let obj_line = lines[i];
                            if !obj_line.starts_with("    ") || obj_line.trim().is_empty() {
                                if obj_line.trim() == "=" {
                                    break;
                                }
                                if !obj_line.starts_with("  ") && !obj_line.trim().is_empty() {
                                    break;
                                }
                                i += 1;
                                continue;
                            }
                            let obj_trimmed = obj_line.trim();
                            if let Some((k, v)) = obj_trimmed.split_once(" = ") {
                                obj.insert(k.trim().to_string(), ConlValue::String(v.trim().to_string()));
                            } else if !obj_trimmed.contains(" = ") && !obj_trimmed.starts_with("=") {
                                // Nested array within object
                                let nested_key = obj_trimmed;
                                let mut nested_arr = Vec::new();
                                i += 1;
                                while i < lines.len() {
                                    let nested_line = lines[i];
                                    if !nested_line.starts_with("      ") {
                                        break;
                                    }
                                    let nested_trimmed = nested_line.trim();
                                    if let Some(val) = nested_trimmed.strip_prefix("= ") {
                                        nested_arr.push(val.to_string());
                                    }
                                    i += 1;
                                }
                                obj.insert(nested_key.to_string(), ConlValue::Array(nested_arr));
                                continue;
                            }
                            i += 1;
                        }
                        if !obj.is_empty() {
                            objects.push(obj);
                        }
                    } else {
                        i += 1;
                    }
                }
                result.insert(key.to_string(), ConlValue::ObjectArray(objects));
            } else if is_array {
                // Simple array
                let mut arr = Vec::new();
                while i < lines.len() {
                    let arr_line = lines[i];
                    if !arr_line.starts_with("  ") && !arr_line.trim().is_empty() {
                        break;
                    }
                    let arr_trimmed = arr_line.trim();
                    if let Some(val) = arr_trimmed.strip_prefix("= ") {
                        arr.push(val.to_string());
                    }
                    i += 1;
                }
                result.insert(key.to_string(), ConlValue::Array(arr));
            } else {
                // Nested object (like credits)
                let mut obj = BTreeMap::new();
                while i < lines.len() {
                    let obj_line = lines[i];
                    if !obj_line.starts_with("  ") && !obj_line.trim().is_empty() {
                        break;
                    }
                    let obj_trimmed = obj_line.trim();
                    if obj_trimmed.is_empty() {
                        i += 1;
                        continue;
                    }
                    if let Some((k, v)) = obj_trimmed.split_once(" = ") {
                        obj.insert(k.trim().to_string(), ConlValue::String(v.trim().to_string()));
                    } else if !obj_trimmed.contains(" = ") {
                        // Nested array (like sources)
                        let nested_key = obj_trimmed;
                        let mut nested_arr = Vec::new();
                        i += 1;
                        while i < lines.len() {
                            let nested_line = lines[i];
                            if !nested_line.starts_with("    ") {
                                break;
                            }
                            let nested_trimmed = nested_line.trim();
                            if let Some(val) = nested_trimmed.strip_prefix("= ") {
                                nested_arr.push(val.to_string());
                            }
                            i += 1;
                        }
                        obj.insert(nested_key.to_string(), ConlValue::Array(nested_arr));
                        continue;
                    }
                    i += 1;
                }
                result.insert(key.to_string(), ConlValue::Object(obj));
            }
        } else {
            i += 1;
        }
    }

    Ok(result)
}

#[derive(Debug, Clone)]
enum ConlValue {
    String(String),
    Array(Vec<String>),
    Object(BTreeMap<String, ConlValue>),
    ObjectArray(Vec<BTreeMap<String, ConlValue>>),
}

impl ConlValue {
    fn as_str(&self) -> Option<&str> {
        if let ConlValue::String(s) = self {
            Some(s)
        } else {
            None
        }
    }

    fn as_array(&self) -> Option<&Vec<String>> {
        if let ConlValue::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }

    fn as_object(&self) -> Option<&BTreeMap<String, ConlValue>> {
        if let ConlValue::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }

    fn as_object_array(&self) -> Option<&Vec<BTreeMap<String, ConlValue>>> {
        if let ConlValue::ObjectArray(a) = self {
            Some(a)
        } else {
            None
        }
    }
}

/// Load a stamp from its metadata.conl file
fn load_stamp(conl_path: &Path) -> Result<Stamp> {
    let content = fs::read_to_string(conl_path)
        .with_context(|| format!("Failed to read {}", conl_path.display()))?;
    let data = parse_conl(&content)?;

    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
    let slug = data.get("slug").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let year = data.get("year").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
    let issue_date = data.get("issue_date").and_then(|v| v.as_str()).map(String::from);
    let issue_location = data.get("issue_location").and_then(|v| v.as_str()).map(String::from);
    let rate = data.get("rate").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
    let rate_type = data.get("rate_type").and_then(|v| v.as_str()).map(String::from);
    let stamp_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("stamp").to_string();
    let series = data.get("series").and_then(|v| v.as_str()).map(String::from);
    let stamp_images = data.get("stamp_images").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let sheet_image = data.get("sheet_image").and_then(|v| v.as_str()).map(String::from);
    let about = data.get("about").and_then(|v| v.as_str()).map(String::from);

    // Parse credits
    let mut credits = Credits::default();
    if let Some(credits_obj) = data.get("credits").and_then(|v| v.as_object()) {
        credits.art_director = credits_obj.get("art_director").and_then(|v| v.as_str()).map(String::from);
        credits.artist = credits_obj.get("artist").and_then(|v| v.as_str()).map(String::from);
        credits.designer = credits_obj.get("designer").and_then(|v| v.as_str()).map(String::from);
        credits.typographer = credits_obj.get("typographer").and_then(|v| v.as_str()).map(String::from);
        credits.photographer = credits_obj.get("photographer").and_then(|v| v.as_str()).map(String::from);
        credits.illustrator = credits_obj.get("illustrator").and_then(|v| v.as_str()).map(String::from);
        credits.sources = credits_obj.get("sources").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    }

    // Parse products
    let mut products = Vec::new();
    if let Some(products_arr) = data.get("products").and_then(|v| v.as_object_array()) {
        for prod in products_arr {
            let title = prod.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let long_title = prod.get("long_title").and_then(|v| v.as_str()).map(String::from);
            let price = prod.get("price").and_then(|v| v.as_str()).map(String::from);
            let postal_store_url = prod.get("postal_store_url").and_then(|v| v.as_str()).map(String::from);
            let stamps_forever_url = prod.get("stamps_forever_url").and_then(|v| v.as_str()).map(String::from);
            let images = prod.get("images").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            products.push(Product {
                title,
                long_title,
                price,
                postal_store_url,
                stamps_forever_url,
                images,
            });
        }
    }

    Ok(Stamp {
        name,
        slug,
        url,
        year,
        issue_date,
        issue_location,
        rate,
        rate_type,
        stamp_type,
        series,
        stamp_images,
        sheet_image,
        credits,
        about,
        products,
    })
}

/// Load all stamps from the data directory
fn load_all_stamps() -> Result<Vec<Stamp>> {
    let mut stamps = Vec::new();
    let data_dir = Path::new(DATA_DIR);

    if !data_dir.exists() {
        return Ok(stamps);
    }

    for year_entry in fs::read_dir(data_dir)? {
        let year_entry = year_entry?;
        let year_path = year_entry.path();

        if !year_path.is_dir() {
            continue;
        }

        let year_name = year_path.file_name().unwrap().to_string_lossy();
        let year: u32 = match year_name.parse() {
            Ok(y) => y,
            Err(_) => continue,
        };

        // Skip years before MIN_YEAR
        if year < MIN_YEAR {
            continue;
        }

        for stamp_entry in fs::read_dir(&year_path)? {
            let stamp_entry = stamp_entry?;
            let stamp_path = stamp_entry.path();

            if !stamp_path.is_dir() {
                continue;
            }

            let conl_path = stamp_path.join("metadata.conl");
            if !conl_path.exists() {
                continue;
            }

            match load_stamp(&conl_path) {
                Ok(stamp) => {
                    // Filter out hidden rate types
                    if let Some(ref rt) = stamp.rate_type {
                        if HIDDEN_RATE_TYPES.contains(&rt.as_str()) {
                            continue;
                        }
                    }
                    stamps.push(stamp);
                }
                Err(e) => {
                    eprintln!("Warning: Failed to load {}: {}", conl_path.display(), e);
                }
            }
        }
    }

    // Sort by year (desc), then issue_date (desc), then name
    stamps.sort_by(|a, b| {
        b.year.cmp(&a.year)
            .then_with(|| b.issue_date.cmp(&a.issue_date))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(stamps)
}

// HTML generation helpers
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn markdown_to_html(md: &str) -> String {
    // Simple markdown to HTML conversion
    let mut html = String::new();
    let paragraphs: Vec<&str> = md.split("\n\n").collect();

    for p in paragraphs {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }

        // Convert *text* to <em>text</em> and **text** to <strong>text</strong>
        let mut converted = p.to_string();

        // Bold first (so we don't interfere with italic detection)
        while let Some(start) = converted.find("**") {
            if let Some(end) = converted[start + 2..].find("**") {
                let end = start + 2 + end;
                let inner = &converted[start + 2..end];
                converted = format!(
                    "{}<strong>{}</strong>{}",
                    &converted[..start],
                    inner,
                    &converted[end + 2..]
                );
            } else {
                break;
            }
        }

        // Italic
        while let Some(start) = converted.find('*') {
            if let Some(end) = converted[start + 1..].find('*') {
                let end = start + 1 + end;
                let inner = &converted[start + 1..end];
                converted = format!(
                    "{}<em>{}</em>{}",
                    &converted[..start],
                    inner,
                    &converted[end + 1..]
                );
            } else {
                break;
            }
        }

        html.push_str(&format!("<p>{}</p>\n", converted));
    }

    html
}

/// CSS styles for the site
fn css_styles() -> &'static str {
    r#"
:root {
    --primary: #1a365d;
    --primary-light: #2a4a7f;
    --accent: #c53030;
    --bg: #f7fafc;
    --card-bg: #ffffff;
    --text: #1a202c;
    --text-muted: #718096;
    --border: #e2e8f0;
    --shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1), 0 2px 4px -1px rgba(0, 0, 0, 0.06);
    --shadow-lg: 0 10px 15px -3px rgba(0, 0, 0, 0.1), 0 4px 6px -2px rgba(0, 0, 0, 0.05);
    --radius: 8px;
}

* {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
}

body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
    background: var(--bg);
    color: var(--text);
    line-height: 1.6;
}

.container {
    max-width: 1400px;
    margin: 0 auto;
    padding: 0 24px;
}

/* Header */
header {
    background: linear-gradient(135deg, var(--primary) 0%, var(--primary-light) 100%);
    color: white;
    padding: 24px 0;
    box-shadow: var(--shadow);
}

header h1 {
    font-size: 1.75rem;
    font-weight: 700;
    letter-spacing: -0.025em;
}

header h1 a {
    color: white;
    text-decoration: none;
}

header nav {
    margin-top: 16px;
    display: flex;
    gap: 24px;
    flex-wrap: wrap;
}

header nav a {
    color: rgba(255, 255, 255, 0.9);
    text-decoration: none;
    font-size: 0.875rem;
    font-weight: 500;
    transition: color 0.2s;
}

header nav a:hover {
    color: white;
}

/* Main content */
main {
    padding: 48px 0;
}

h2 {
    font-size: 1.5rem;
    font-weight: 700;
    margin-bottom: 24px;
    color: var(--primary);
}

h3 {
    font-size: 1.25rem;
    font-weight: 600;
    margin-bottom: 16px;
    color: var(--text);
}

/* Stamp grid */
.stamp-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 24px;
    margin-bottom: 48px;
}

.stamp-card {
    background: var(--card-bg);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    overflow: hidden;
    transition: transform 0.2s, box-shadow 0.2s;
}

.stamp-card:hover {
    transform: translateY(-4px);
    box-shadow: var(--shadow-lg);
}

.stamp-card a {
    text-decoration: none;
    color: inherit;
    display: block;
}

.stamp-card-image {
    aspect-ratio: 4/3;
    background: #f0f0f0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
}

.stamp-card-image img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    padding: 16px;
}

.stamp-card-content {
    padding: 16px;
}

.stamp-card-title {
    font-weight: 600;
    font-size: 1rem;
    margin-bottom: 4px;
    color: var(--text);
}

.stamp-card-meta {
    font-size: 0.875rem;
    color: var(--text-muted);
}

.stamp-card-rate {
    display: inline-block;
    background: var(--primary);
    color: white;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
    margin-top: 8px;
}

.stamp-card-rate.available {
    background: #38a169;
}

/* Stamp detail page */
.stamp-detail {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 48px;
    margin-bottom: 48px;
}

@media (max-width: 768px) {
    .stamp-detail {
        grid-template-columns: 1fr;
    }
}

.stamp-images {
    display: flex;
    flex-direction: column;
    gap: 24px;
}

.stamp-main-image {
    background: var(--card-bg);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
}

.stamp-main-image img {
    max-width: 100%;
    max-height: 400px;
    object-fit: contain;
}

.stamp-thumbnails {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
}

.stamp-thumbnails img {
    width: 80px;
    height: 80px;
    object-fit: contain;
    background: var(--card-bg);
    border-radius: 4px;
    padding: 8px;
    cursor: pointer;
    border: 2px solid transparent;
    transition: border-color 0.2s;
}

.stamp-thumbnails img:hover {
    border-color: var(--primary);
}

.stamp-info {
    background: var(--card-bg);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 32px;
}

.stamp-info h1 {
    font-size: 2rem;
    font-weight: 700;
    margin-bottom: 16px;
    color: var(--text);
}

.stamp-meta-grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 16px;
    margin-bottom: 24px;
    font-size: 0.9375rem;
}

.stamp-meta-label {
    font-weight: 600;
    color: var(--text-muted);
}

.stamp-about {
    margin-top: 24px;
    padding-top: 24px;
    border-top: 1px solid var(--border);
}

.stamp-about p {
    margin-bottom: 16px;
}

/* Products section */
.products-section {
    margin-top: 48px;
}

.products-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 24px;
}

.product-card {
    background: var(--card-bg);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    overflow: hidden;
    transition: transform 0.2s, box-shadow 0.2s;
}

.product-card:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-lg);
}

.product-card-image {
    aspect-ratio: 16/9;
    background: #f0f0f0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
}

.product-card-image img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
}

.product-card-content {
    padding: 16px;
}

.product-card-title {
    font-weight: 600;
    font-size: 1rem;
    margin-bottom: 8px;
}

.product-card-price {
    font-size: 1.25rem;
    font-weight: 700;
    color: var(--accent);
    margin-bottom: 12px;
}

.product-card-link {
    display: inline-block;
    background: var(--primary);
    color: white;
    padding: 8px 16px;
    border-radius: 4px;
    text-decoration: none;
    font-size: 0.875rem;
    font-weight: 500;
    transition: background 0.2s;
}

.product-card-link:hover {
    background: var(--primary-light);
}

/* Year navigation */
.year-nav {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 32px;
}

.year-nav a {
    display: inline-block;
    padding: 8px 16px;
    background: var(--card-bg);
    border-radius: 4px;
    text-decoration: none;
    color: var(--text);
    font-weight: 500;
    box-shadow: var(--shadow);
    transition: background 0.2s, color 0.2s;
}

.year-nav a:hover, .year-nav a.active {
    background: var(--primary);
    color: white;
}

/* Section divider */
.section-divider {
    margin: 48px 0;
    border: 0;
    border-top: 1px solid var(--border);
}

/* Breadcrumb */
.breadcrumb {
    display: flex;
    gap: 8px;
    margin-bottom: 24px;
    font-size: 0.875rem;
    color: var(--text-muted);
}

.breadcrumb a {
    color: var(--primary);
    text-decoration: none;
}

.breadcrumb a:hover {
    text-decoration: underline;
}

/* Category badges */
.category-badge {
    display: inline-block;
    padding: 4px 12px;
    border-radius: 999px;
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
}

.category-badge.forever { background: #e6f6e6; color: #22543d; }
.category-badge.global { background: #e6f0ff; color: #1a365d; }
.category-badge.postcard { background: #fef3c7; color: #92400e; }
.category-badge.additional { background: #e9d8fd; color: #553c9a; }
.category-badge.denominated { background: #fed7e2; color: #97266d; }

/* People index */
.people-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr));
    gap: 16px;
}

.person-link {
    display: block;
    padding: 16px;
    background: var(--card-bg);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    text-decoration: none;
    color: var(--text);
    transition: transform 0.2s, box-shadow 0.2s;
}

.person-link:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-lg);
}

.person-name {
    font-weight: 600;
    margin-bottom: 4px;
}

.person-count {
    font-size: 0.875rem;
    color: var(--text-muted);
}

/* Footer */
footer {
    background: var(--primary);
    color: rgba(255, 255, 255, 0.8);
    padding: 32px 0;
    margin-top: 64px;
    text-align: center;
    font-size: 0.875rem;
}

footer a {
    color: white;
}

/* Discontinued section */
.discontinued-section {
    opacity: 0.7;
}

.discontinued-label {
    background: var(--text-muted);
    color: white;
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
}
"#
}

/// Generate page header HTML
fn page_header(title: &str, current_path: &str) -> String {
    let nav_items = [
        ("/", "Home"),
        ("/forever-stamps/", "Forever"),
        ("/postcard-forever-stamps/", "Postcard"),
        ("/global-forever-stamps/", "Global"),
        ("/additional-postage-forever-stamps/", "Additional"),
        ("/denominated-postage-stamps/", "Denominated"),
        ("/cards/", "Cards"),
        ("/envelopes/", "Envelopes"),
        ("/people/", "People"),
    ];

    let nav_html: String = nav_items
        .iter()
        .map(|(path, label)| {
            let active = if *path == current_path { " class=\"active\"" } else { "" };
            format!("<a href=\"{}\"{}>{}  </a>", path, active, label)
        })
        .collect();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{} - US Postage Stamps</title>
    <style>{}</style>
</head>
<body>
    <header>
        <div class="container">
            <h1><a href="/">US Postage Stamps</a></h1>
            <nav>{}</nav>
        </div>
    </header>
    <main>
        <div class="container">
"#,
        html_escape(title),
        css_styles(),
        nav_html
    )
}

/// Generate page footer HTML
fn page_footer() -> &'static str {
    r#"
        </div>
    </main>
    <footer>
        <div class="container">
            <p>Stamp data from <a href="https://stampsforever.com">StampsForever.com</a></p>
        </div>
    </footer>
</body>
</html>
"#
}

/// Generate a stamp card HTML
fn stamp_card_html(stamp: &Stamp, image_base: &str) -> String {
    let image_html = if let Some(img) = stamp.stamp_images.first() {
        format!(
            r#"<img src="{}/{}/{}/{}" alt="{}">"#,
            image_base, stamp.year, stamp.slug, img, html_escape(&stamp.name)
        )
    } else if let Some(img) = &stamp.sheet_image {
        format!(
            r#"<img src="{}/{}/{}/{}" alt="{}">"#,
            image_base, stamp.year, stamp.slug, img, html_escape(&stamp.name)
        )
    } else {
        "<span>No image</span>".to_string()
    };

    let rate_html = if let Some(rate) = stamp.rate {
        let rate_str = format_rate(rate);
        let available_class = if !stamp.products.is_empty() { " available" } else { "" };
        format!(r#"<span class="stamp-card-rate{}">{}</span>"#, available_class, rate_str)
    } else {
        String::new()
    };

    format!(
        r#"<div class="stamp-card">
    <a href="/{}/{}/">
        <div class="stamp-card-image">{}</div>
        <div class="stamp-card-content">
            <div class="stamp-card-title">{}</div>
            <div class="stamp-card-meta">{}</div>
            {}
        </div>
    </a>
</div>"#,
        stamp.year,
        stamp.slug,
        image_html,
        html_escape(&stamp.name),
        stamp.year,
        rate_html
    )
}

/// Generate an individual stamp page
fn generate_stamp_page(stamp: &Stamp, output_dir: &Path) -> Result<()> {
    let page_dir = output_dir.join(stamp.year.to_string()).join(&stamp.slug);
    fs::create_dir_all(&page_dir)?;

    let mut html = page_header(&stamp.name, "");

    // Breadcrumb
    html.push_str(&format!(
        r#"<nav class="breadcrumb">
    <a href="/">Home</a> <span>/</span>
    <a href="/{}/">{}</a> <span>/</span>
    <span>{}</span>
</nav>
"#,
        stamp.year, stamp.year, html_escape(&stamp.name)
    ));

    // Main content
    html.push_str(r#"<div class="stamp-detail">"#);

    // Images column
    html.push_str(r#"<div class="stamp-images">"#);

    // Main image
    let main_image = stamp.stamp_images.first().or(stamp.sheet_image.as_ref());
    if let Some(img) = main_image {
        html.push_str(&format!(
            r#"<div class="stamp-main-image">
    <img src="/images/{}/{}/{}" alt="{}">
</div>"#,
            stamp.year, stamp.slug, img, html_escape(&stamp.name)
        ));
    }

    // Thumbnails
    if stamp.stamp_images.len() > 1 || (stamp.stamp_images.len() == 1 && stamp.sheet_image.is_some()) {
        html.push_str(r#"<div class="stamp-thumbnails">"#);
        for img in &stamp.stamp_images {
            html.push_str(&format!(
                r#"<img src="/images/{}/{}/{}" alt="Stamp variant">"#,
                stamp.year, stamp.slug, img
            ));
        }
        if let Some(sheet) = &stamp.sheet_image {
            html.push_str(&format!(
                r#"<img src="/images/{}/{}/{}" alt="Stamp sheet">"#,
                stamp.year, stamp.slug, sheet
            ));
        }
        html.push_str("</div>");
    }

    html.push_str("</div>"); // stamp-images

    // Info column
    html.push_str(r#"<div class="stamp-info">"#);
    html.push_str(&format!("<h1>{}</h1>", html_escape(&stamp.name)));

    // Meta grid
    html.push_str(r#"<div class="stamp-meta-grid">"#);

    html.push_str(&format!(
        r#"<span class="stamp-meta-label">Year</span><span><a href="/{}/">{}</a></span>"#,
        stamp.year, stamp.year
    ));

    if let Some(date) = &stamp.issue_date {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Issue Date</span><span>{}</span>"#,
            date
        ));
    }

    if let Some(loc) = &stamp.issue_location {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Location</span><span>{}</span>"#,
            html_escape(loc)
        ));
    }

    if let Some(rate) = stamp.rate {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Rate</span><span>{}</span>"#,
            format_rate(rate)
        ));
    }

    if let Some(rate_type) = &stamp.rate_type {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Type</span><span>{}</span>"#,
            html_escape(rate_type)
        ));
    }

    if let Some(series) = &stamp.series {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Series</span><span>{}</span>"#,
            html_escape(series)
        ));
    }

    // Credits
    if let Some(ad) = &stamp.credits.art_director {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Art Director</span><span><a href="/people/{}/">{}</a></span>"#,
            slugify(ad), html_escape(ad)
        ));
    }
    if let Some(artist) = &stamp.credits.artist {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Artist</span><span><a href="/people/{}/">{}</a></span>"#,
            slugify(artist), html_escape(artist)
        ));
    }
    if let Some(designer) = &stamp.credits.designer {
        if stamp.credits.artist.as_deref() != Some(designer) {
            html.push_str(&format!(
                r#"<span class="stamp-meta-label">Designer</span><span><a href="/people/{}/">{}</a></span>"#,
                slugify(designer), html_escape(designer)
            ));
        }
    }
    if let Some(photographer) = &stamp.credits.photographer {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Photographer</span><span><a href="/people/{}/">{}</a></span>"#,
            slugify(photographer), html_escape(photographer)
        ));
    }
    if let Some(illustrator) = &stamp.credits.illustrator {
        html.push_str(&format!(
            r#"<span class="stamp-meta-label">Illustrator</span><span><a href="/people/{}/">{}</a></span>"#,
            slugify(illustrator), html_escape(illustrator)
        ));
    }

    html.push_str("</div>"); // stamp-meta-grid

    // About
    if let Some(about) = &stamp.about {
        html.push_str(r#"<div class="stamp-about">"#);
        html.push_str(&markdown_to_html(about));
        html.push_str("</div>");
    }

    // External links
    html.push_str(r#"<div style="margin-top: 24px; padding-top: 24px; border-top: 1px solid var(--border);">"#);
    html.push_str(&format!(
        r#"<a href="{}" target="_blank" rel="noopener" style="color: var(--primary); margin-right: 16px;">View on StampsForever.com</a>"#,
        stamp.url
    ));
    html.push_str("</div>");

    html.push_str("</div>"); // stamp-info
    html.push_str("</div>"); // stamp-detail

    // Products section
    if !stamp.products.is_empty() {
        html.push_str(r#"<section class="products-section">"#);
        html.push_str("<h2>Available Products</h2>");
        html.push_str(r#"<div class="products-grid">"#);

        for product in &stamp.products {
            html.push_str(r#"<div class="product-card">"#);

            if let Some(img) = product.images.first() {
                html.push_str(&format!(
                    r#"<div class="product-card-image"><img src="/images/{}/{}/{}" alt="{}"></div>"#,
                    stamp.year, stamp.slug, img, html_escape(&product.title)
                ));
            }

            html.push_str(r#"<div class="product-card-content">"#);
            html.push_str(&format!(
                r#"<div class="product-card-title">{}</div>"#,
                html_escape(&product.title)
            ));

            if let Some(price) = &product.price {
                html.push_str(&format!(
                    r#"<div class="product-card-price">{}</div>"#,
                    html_escape(price)
                ));
            }

            if let Some(url) = &product.postal_store_url {
                html.push_str(&format!(
                    r#"<a href="{}" target="_blank" rel="noopener" class="product-card-link">Buy at USPS</a>"#,
                    url
                ));
            }

            html.push_str("</div></div>");
        }

        html.push_str("</div></section>");
    }

    html.push_str(page_footer());

    let page_path = page_dir.join("index.html");
    fs::write(&page_path, html)?;

    Ok(())
}

/// Generate year index page
fn generate_year_page(year: u32, stamps: &[&Stamp], output_dir: &Path) -> Result<()> {
    let page_dir = output_dir.join(year.to_string());
    fs::create_dir_all(&page_dir)?;

    let mut html = page_header(&format!("{} Stamps", year), "");

    // Breadcrumb
    html.push_str(&format!(
        r#"<nav class="breadcrumb">
    <a href="/">Home</a> <span>/</span>
    <span>{}</span>
</nav>
"#,
        year
    ));

    html.push_str(&format!("<h2>{} Stamps</h2>", year));
    html.push_str(&format!("<p style=\"margin-bottom: 24px; color: var(--text-muted);\">{} stamps issued</p>", stamps.len()));

    // Group by denomination category
    let mut by_category: BTreeMap<String, Vec<&Stamp>> = BTreeMap::new();
    for stamp in stamps {
        let cat = DenominationCategory::from_stamp(stamp);
        let cat_name = cat.display_name().to_string();
        by_category.entry(cat_name).or_default().push(stamp);
    }

    for (cat_name, cat_stamps) in &by_category {
        html.push_str(&format!("<h3>{}</h3>", cat_name));
        html.push_str(r#"<div class="stamp-grid">"#);
        for stamp in cat_stamps {
            html.push_str(&stamp_card_html(stamp, "/images"));
        }
        html.push_str("</div>");
    }

    html.push_str(page_footer());

    let page_path = page_dir.join("index.html");
    fs::write(&page_path, html)?;

    Ok(())
}

/// Generate a category page (forever stamps, etc.)
fn generate_category_page(
    category: &str,
    title: &str,
    filter_fn: impl Fn(&Stamp) -> bool,
    stamps: &[Stamp],
    output_dir: &Path,
) -> Result<()> {
    let page_dir = output_dir.join(category);
    fs::create_dir_all(&page_dir)?;

    let filtered: Vec<&Stamp> = stamps.iter().filter(|s| filter_fn(s)).collect();
    let total_count = filtered.len();

    // Split into available (has products) and discontinued
    let (available, discontinued): (Vec<&Stamp>, Vec<&Stamp>) = filtered
        .into_iter()
        .partition(|s| !s.products.is_empty());

    let mut html = page_header(title, &format!("/{}/", category));

    // Breadcrumb
    html.push_str(&format!(
        r#"<nav class="breadcrumb">
    <a href="/">Home</a> <span>/</span>
    <span>{}</span>
</nav>
"#,
        title
    ));

    html.push_str(&format!("<h2>{}</h2>", title));
    html.push_str(&format!(
        "<p style=\"margin-bottom: 24px; color: var(--text-muted);\">{} stamps ({} available, {} discontinued)</p>",
        total_count, available.len(), discontinued.len()
    ));

    // Available stamps
    if !available.is_empty() {
        html.push_str("<h3>Currently Available</h3>");
        html.push_str(r#"<div class="stamp-grid">"#);
        for stamp in &available {
            html.push_str(&stamp_card_html(stamp, "/images"));
        }
        html.push_str("</div>");
    }

    // Discontinued stamps
    if !discontinued.is_empty() {
        html.push_str(r#"<hr class="section-divider">"#);
        html.push_str(r#"<div class="discontinued-section">"#);
        html.push_str("<h3>Discontinued</h3>");
        html.push_str(r#"<div class="stamp-grid">"#);
        for stamp in &discontinued {
            html.push_str(&stamp_card_html(stamp, "/images"));
        }
        html.push_str("</div></div>");
    }

    html.push_str(page_footer());

    let page_path = page_dir.join("index.html");
    fs::write(&page_path, html)?;

    Ok(())
}

/// Slugify a name for URL use
fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Generate people index and individual pages
fn generate_people_pages(stamps: &[Stamp], output_dir: &Path) -> Result<()> {
    // Collect all people and their stamps
    let mut people: HashMap<String, Vec<&Stamp>> = HashMap::new();

    for stamp in stamps {
        if let Some(name) = &stamp.credits.art_director {
            people.entry(name.clone()).or_default().push(stamp);
        }
        if let Some(name) = &stamp.credits.artist {
            people.entry(name.clone()).or_default().push(stamp);
        }
        if let Some(name) = &stamp.credits.designer {
            if stamp.credits.artist.as_deref() != Some(name) {
                people.entry(name.clone()).or_default().push(stamp);
            }
        }
        if let Some(name) = &stamp.credits.photographer {
            people.entry(name.clone()).or_default().push(stamp);
        }
        if let Some(name) = &stamp.credits.illustrator {
            people.entry(name.clone()).or_default().push(stamp);
        }
        for source in &stamp.credits.sources {
            people.entry(source.clone()).or_default().push(stamp);
        }
    }

    // Sort by name
    let mut sorted_people: Vec<_> = people.into_iter().collect();
    sorted_people.sort_by(|a, b| a.0.cmp(&b.0));

    // Generate index page
    let people_dir = output_dir.join("people");
    fs::create_dir_all(&people_dir)?;

    let mut html = page_header("People", "/people/");

    html.push_str(r#"<nav class="breadcrumb">
    <a href="/">Home</a> <span>/</span>
    <span>People</span>
</nav>
"#);

    html.push_str("<h2>Artists, Designers & Photographers</h2>");
    html.push_str(&format!(
        "<p style=\"margin-bottom: 24px; color: var(--text-muted);\">{} people</p>",
        sorted_people.len()
    ));

    html.push_str(r#"<div class="people-grid">"#);
    for (name, person_stamps) in &sorted_people {
        let slug = slugify(name);
        // Deduplicate stamps
        let unique_stamps: HashSet<_> = person_stamps.iter().map(|s| &s.slug).collect();
        html.push_str(&format!(
            r#"<a href="/people/{}/" class="person-link">
    <div class="person-name">{}</div>
    <div class="person-count">{} stamps</div>
</a>"#,
            slug, html_escape(name), unique_stamps.len()
        ));
    }
    html.push_str("</div>");

    html.push_str(page_footer());
    fs::write(people_dir.join("index.html"), html)?;

    // Generate individual person pages
    for (name, person_stamps) in &sorted_people {
        let slug = slugify(name);
        let person_dir = people_dir.join(&slug);
        fs::create_dir_all(&person_dir)?;

        let mut html = page_header(name, "");

        html.push_str(&format!(
            r#"<nav class="breadcrumb">
    <a href="/">Home</a> <span>/</span>
    <a href="/people/">People</a> <span>/</span>
    <span>{}</span>
</nav>
"#,
            html_escape(name)
        ));

        // Deduplicate and sort stamps
        let mut unique_stamps: Vec<_> = person_stamps.iter().collect();
        unique_stamps.sort_by(|a, b| b.year.cmp(&a.year).then_with(|| a.name.cmp(&b.name)));
        unique_stamps.dedup_by(|a, b| a.slug == b.slug);

        html.push_str(&format!("<h2>{}</h2>", html_escape(name)));
        html.push_str(&format!(
            "<p style=\"margin-bottom: 24px; color: var(--text-muted);\">{} stamps</p>",
            unique_stamps.len()
        ));

        html.push_str(r#"<div class="stamp-grid">"#);
        for stamp in &unique_stamps {
            html.push_str(&stamp_card_html(stamp, "/images"));
        }
        html.push_str("</div>");

        html.push_str(page_footer());
        fs::write(person_dir.join("index.html"), html)?;
    }

    Ok(())
}

/// Generate homepage
fn generate_homepage(stamps: &[Stamp], years: &[u32], output_dir: &Path) -> Result<()> {
    let mut html = page_header("US Postage Stamps", "/");

    html.push_str("<h2>US Postage Stamps</h2>");
    html.push_str(&format!(
        "<p style=\"margin-bottom: 24px; color: var(--text-muted);\">{} stamps from {} to {}</p>",
        stamps.len(),
        years.last().unwrap_or(&2007),
        years.first().unwrap_or(&2026)
    ));

    // Year navigation
    html.push_str(r#"<div class="year-nav">"#);
    for year in years {
        html.push_str(&format!(r#"<a href="/{}/">{}</a>"#, year, year));
    }
    html.push_str("</div>");

    // Show recent stamps (last 2 years)
    let current_year = years.first().copied().unwrap_or(2026);
    let recent: Vec<_> = stamps.iter().filter(|s| s.year >= current_year - 1).collect();

    html.push_str("<h3>Recent Stamps</h3>");
    html.push_str(r#"<div class="stamp-grid">"#);
    for stamp in recent.iter().take(24) {
        html.push_str(&stamp_card_html(stamp, "/images"));
    }
    html.push_str("</div>");

    html.push_str(page_footer());

    fs::write(output_dir.join("index.html"), html)?;

    Ok(())
}

/// Create symlinks for images
fn symlink_images(stamps: &[Stamp], output_dir: &Path) -> Result<()> {
    let images_dir = output_dir.join("images");
    fs::create_dir_all(&images_dir)?;

    let data_dir = Path::new(DATA_DIR);

    for stamp in stamps {
        let stamp_images_dir = images_dir.join(stamp.year.to_string()).join(&stamp.slug);
        let source_dir = data_dir.join(stamp.year.to_string()).join(&stamp.slug);

        if !source_dir.exists() {
            continue;
        }

        fs::create_dir_all(&stamp_images_dir)?;

        // Link all image files
        for entry in fs::read_dir(&source_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ["png", "jpg", "jpeg", "gif", "webp"].contains(&ext.to_lowercase().as_str()) {
                    let filename = path.file_name().unwrap();
                    let link_path = stamp_images_dir.join(filename);

                    // Remove existing symlink if present
                    if link_path.exists() || link_path.is_symlink() {
                        fs::remove_file(&link_path).ok();
                    }

                    // Create symlink (use absolute path for source)
                    let abs_source = fs::canonicalize(&path)?;
                    symlink(&abs_source, &link_path)?;
                }
            }
        }
    }

    Ok(())
}

/// Main generation function
pub fn run_generate() -> Result<()> {
    println!("Loading stamps...");
    let stamps = load_all_stamps()?;
    println!("Loaded {} stamps", stamps.len());

    if stamps.is_empty() {
        println!("No stamps found. Run 'usps-rates stamps scrape-details' first.");
        return Ok(());
    }

    let output_dir = PathBuf::from(OUTPUT_DIR);

    // Clean and create output directory
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)?;
    }
    fs::create_dir_all(&output_dir)?;

    // Collect years
    let mut years: Vec<u32> = stamps.iter().map(|s| s.year).collect::<HashSet<_>>().into_iter().collect();
    years.sort_by(|a, b| b.cmp(a)); // Descending

    println!("Generating stamp pages...");
    for stamp in &stamps {
        generate_stamp_page(stamp, &output_dir)?;
    }

    println!("Generating year pages...");
    for year in &years {
        let year_stamps: Vec<_> = stamps.iter().filter(|s| s.year == *year).collect();
        generate_year_page(*year, &year_stamps, &output_dir)?;
    }

    println!("Generating category pages...");

    // Forever stamps
    generate_category_page(
        "forever-stamps",
        "Forever Stamps",
        |s| matches!(
            s.rate_type.as_deref(),
            Some("Forever") | Some("Semipostal")
        ) && s.stamp_type == "stamp",
        &stamps,
        &output_dir,
    )?;

    // Additional postage forever stamps
    generate_category_page(
        "additional-postage-forever-stamps",
        "Additional Postage Forever Stamps",
        |s| matches!(
            s.rate_type.as_deref(),
            Some("Additional Ounce") | Some("Two Ounce") | Some("Three Ounce") | Some("Additional Postage")
        ),
        &stamps,
        &output_dir,
    )?;

    // Non-machinable forever stamps
    generate_category_page(
        "non-machinable-forever-stamps",
        "Non-Machinable Forever Stamps",
        |s| s.rate_type.as_deref() == Some("Nonmachineable Surcharge"),
        &stamps,
        &output_dir,
    )?;

    // Global forever stamps
    generate_category_page(
        "global-forever-stamps",
        "Global Forever Stamps",
        |s| matches!(
            s.rate_type.as_deref(),
            Some("International") | Some("Global Forever")
        ),
        &stamps,
        &output_dir,
    )?;

    // Postcard forever stamps
    generate_category_page(
        "postcard-forever-stamps",
        "Postcard Forever Stamps",
        |s| s.rate_type.as_deref() == Some("Postcard"),
        &stamps,
        &output_dir,
    )?;

    // Denominated postage stamps
    generate_category_page(
        "denominated-postage-stamps",
        "Denominated Postage Stamps",
        |s| {
            matches!(
                s.rate_type.as_deref(),
                Some("Definitive") | Some("Other Denomination") | Some("First Class") | Some("Special")
            ) || extract_denomination(&s.name).is_some()
        },
        &stamps,
        &output_dir,
    )?;

    // Cards
    generate_category_page(
        "cards",
        "Stamped Cards",
        |s| s.stamp_type == "card",
        &stamps,
        &output_dir,
    )?;

    // Envelopes
    generate_category_page(
        "envelopes",
        "Stamped Envelopes",
        |s| s.stamp_type == "envelope",
        &stamps,
        &output_dir,
    )?;

    println!("Generating people pages...");
    generate_people_pages(&stamps, &output_dir)?;

    println!("Generating homepage...");
    generate_homepage(&stamps, &years, &output_dir)?;

    println!("Creating image symlinks...");
    symlink_images(&stamps, &output_dir)?;

    println!("Done! Generated site in {}/", OUTPUT_DIR);

    Ok(())
}
