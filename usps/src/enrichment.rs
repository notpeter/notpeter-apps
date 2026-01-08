use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};

const ENRICHMENT_DIR: &str = "enrichment/images";
const LOGS_DIR: &str = "logs";
const PRICING_FILE: &str = "data/llms/model_prices_and_context_window.json";
const PRICING_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json";
const PRICING_MAX_AGE_DAYS: u64 = 7;

const GEMINI_MODEL: &str = "gemini-2.5-flash-lite-preview-09-2025";
const GEMINI_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const PARALLEL_REQUESTS: usize = 5;

/// Stamp enrichment data from AI analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StampEnrichment {
    /// Image filename that was analyzed
    pub image_filename: String,
    /// Year of issue shown on stamp (small text, 4 digits)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// Words/text visible on the stamp
    pub words: Vec<String>,
    /// Keywords describing the visual contents (3-7)
    pub keywords: Vec<String>,
    /// Short description of the stamp image
    pub description: String,
    /// Postal value in cents (e.g., 78 for 78¢, 170 for $1.70)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<i32>,
    /// Value type: "denominated", "forever", "global forever", "postcard forever", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
    /// Mail class: "first class", "priority mail", "priority mail express", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail_class: Option<String>,
    /// Shape: "portrait", "landscape", "square", "circular", "triangle"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    /// Whether the border is non-white (full bleed) or white
    pub full_bleed: bool,
}

/// Usage statistics from API response
#[derive(Debug, Default, Clone)]
struct UsageStats {
    prompt_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
}

impl UsageStats {
    fn add(&mut self, other: &UsageStats) {
        self.prompt_tokens += other.prompt_tokens;
        self.cached_tokens += other.cached_tokens;
        self.output_tokens += other.output_tokens;
    }
}

/// Pricing info for a model
#[derive(Debug, Clone)]
struct ModelPricing {
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    cache_read_cost_per_token: f64,
}

// Gemini API types
#[derive(Debug, Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Debug, Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum GeminiPart {
    Text { text: String },
    InlineData { inline_data: InlineData },
}

#[derive(Debug, Serialize)]
struct InlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    #[serde(rename = "responseMimeType")]
    response_mime_type: String,
    #[serde(rename = "thinkingConfig")]
    thinking_config: ThinkingConfig,
}

#[derive(Debug, Serialize)]
struct ThinkingConfig {
    #[serde(rename = "thinkingBudget")]
    thinking_budget: i32,
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    error: Option<GeminiError>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiResponseContent,
}

#[derive(Debug, Deserialize)]
struct GeminiResponseContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponsePart {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiError {
    message: String,
}

/// Response structure from Gemini for single image analysis
#[derive(Debug, Deserialize)]
struct GeminiAnalysis {
    year: Option<i32>,
    words: Vec<String>,
    keywords: Vec<String>,
    description: String,
    value: Option<i32>,
    value_type: Option<String>,
    mail_class: Option<String>,
    shape: Option<String>,
    full_bleed: bool,
}

fn get_api_key() -> Result<String> {
    std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .context("GEMINI_API_KEY or GOOGLE_API_KEY environment variable must be set")
}

fn get_mime_type(path: &str) -> &'static str {
    match path {
        p if p.ends_with(".png") => "image/png",
        p if p.ends_with(".jpg") || p.ends_with(".jpeg") => "image/jpeg",
        p if p.ends_with(".gif") => "image/gif",
        p if p.ends_with(".webp") => "image/webp",
        _ => "image/png",
    }
}

/// Write JSON with sorted keys, compact arrays, trailing newline
fn write_json_file<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let json_value = serde_json::to_value(value)?;
    let sorted = sort_json_value(json_value);
    let mut json_str = format_json_compact_arrays(&sorted, 0);
    json_str.push('\n');
    fs::write(path, json_str)?;
    Ok(())
}

/// Recursively sort JSON object keys
fn sort_json_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_value(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_json_value).collect()),
        other => other,
    }
}

/// Format JSON with pretty objects but compact arrays
fn format_json_compact_arrays(value: &Value, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut result = String::from("{\n");
            let entries: Vec<_> = map.iter().collect();
            for (i, (k, v)) in entries.iter().enumerate() {
                result.push_str(&"  ".repeat(indent + 1));
                result.push_str(&format!("\"{}\": ", k));
                result.push_str(&format_json_compact_arrays(v, indent + 1));
                if i < entries.len() - 1 {
                    result.push(',');
                }
                result.push('\n');
            }
            result.push_str(&indent_str);
            result.push('}');
            result
        }
        Value::Array(arr) => {
            // Arrays are always compact (single line)
            let items: Vec<String> = arr
                .iter()
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .collect();
            format!("[{}]", items.join(", "))
        }
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// Load or fetch pricing data
fn load_pricing() -> Result<ModelPricing> {
    let pricing_path = PathBuf::from(PRICING_FILE);

    // Check if file exists and is fresh enough
    let needs_update = if pricing_path.exists() {
        let metadata = fs::metadata(&pricing_path)?;
        let modified = metadata.modified()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::MAX);
        age > Duration::from_secs(PRICING_MAX_AGE_DAYS * 24 * 60 * 60)
    } else {
        true
    };

    if needs_update {
        eprintln!("Updating pricing data from LiteLLM...");
        let client = reqwest::blocking::Client::new();
        let response = client.get(PRICING_URL).send()?;
        let content = response.text()?;
        if let Some(parent) = pricing_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&pricing_path, &content)?;
    }

    // Parse pricing file
    let content = fs::read_to_string(&pricing_path)?;
    let pricing: Value = serde_json::from_str(&content)?;

    // Look for our model with gemini/ prefix
    let model_key = format!("gemini/{}", GEMINI_MODEL);
    let model_pricing = pricing
        .get(&model_key)
        .context(format!("Model {} not found in pricing data", model_key))?;

    Ok(ModelPricing {
        input_cost_per_token: model_pricing["input_cost_per_token"]
            .as_f64()
            .unwrap_or(0.0),
        output_cost_per_token: model_pricing["output_cost_per_token"]
            .as_f64()
            .unwrap_or(0.0),
        cache_read_cost_per_token: model_pricing["cache_read_input_token_cost"]
            .as_f64()
            .unwrap_or(0.0),
    })
}

/// Represents an image to be processed
#[derive(Clone)]
struct ImageToProcess {
    image_filename: String,
    image_data: Vec<u8>,
}

/// Analyze a single stamp image (for parallel processing)
fn analyze_single_stamp(
    client: &reqwest::blocking::Client,
    api_key: &str,
    image: &ImageToProcess,
) -> Result<(StampEnrichment, UsageStats)> {
    let base64_image = BASE64_STANDARD.encode(&image.image_data);
    let mime_type = get_mime_type(&image.image_filename);

    let prompt = r#"Analyze this US postage stamp image and provide the following information as a JSON object:

{
  "year": integer or null,
  "words": ["string"],
  "keywords": ["string"],
  "description": "string",
  "value": integer or null,
  "value_type": "string or null",
  "mail_class": "string or null",
  "shape": "string or null",
  "full_bleed": boolean
}

Field descriptions:
- year: Small text year of issue shown on stamp, or null. (four digits, 20th or 21st century)
- words: All visible text/words on the stamp (denomination, "USA", "FOREVER", etc.)
- keywords: 3-7 keywords describing visual contents
- description: Brief 1-2 sentence description of what the stamp depicts
- value: Postal value, in cents, if shown (e.g., "78c" == "78", "1.70" == "170", "$5" == "500"), or null
- value_type: One of: "denominated", "forever", "global forever", "postcard forever", "additional ounce", "two ounce", "three ounce", "nonmachinable", "priority mail", "priority mail express", or null
- mail_class: One of: "first class", "priority mail", "priority mail express", "postcard", "presorted", "airmail", or null
- shape: One of: "portrait", "landscape", "square", "circular", "triangle"
- full_bleed: true if border is non-white (full bleed), false if white border

Respond with ONLY the JSON object."#;

    let request = GeminiRequest {
        contents: vec![GeminiContent {
            parts: vec![
                GeminiPart::InlineData {
                    inline_data: InlineData {
                        mime_type: mime_type.to_string(),
                        data: base64_image,
                    },
                },
                GeminiPart::Text {
                    text: prompt.to_string(),
                },
            ],
        }],
        generation_config: GenerationConfig {
            temperature: 0.1,
            response_mime_type: "application/json".to_string(),
            thinking_config: ThinkingConfig { thinking_budget: 0 },
        },
    };

    let url = format!(
        "{}/{}:generateContent?key={}",
        GEMINI_API_URL, GEMINI_MODEL, api_key
    );

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .context("Failed to send request to Gemini API")?;

    let response_text = response.text().context("Failed to read Gemini response")?;
    let gemini_response: GeminiResponse =
        serde_json::from_str(&response_text).context("Failed to parse Gemini response JSON")?;

    if let Some(error) = gemini_response.error {
        bail!("Gemini API error: {}", error.message);
    }

    let usage = gemini_response
        .usage_metadata
        .as_ref()
        .map_or(UsageStats::default(), |u| UsageStats {
            prompt_tokens: u.prompt_token_count.unwrap_or(0),
            cached_tokens: u.cached_content_token_count.unwrap_or(0),
            output_tokens: u.candidates_token_count.unwrap_or(0),
        });

    let candidates = gemini_response
        .candidates
        .context("No candidates in Gemini response")?;
    let first_candidate = candidates.first().context("Empty candidates array")?;
    let first_part = first_candidate
        .content
        .parts
        .first()
        .context("No parts in response content")?;

    let text = first_part
        .text
        .as_ref()
        .context("No text in response part")?;
    let analysis: GeminiAnalysis = serde_json::from_str(text)
        .with_context(|| format!("Failed to parse analysis JSON: {}", text))?;

    let enrichment = StampEnrichment {
        image_filename: image.image_filename.clone(),
        year: analysis.year,
        words: analysis.words,
        keywords: analysis.keywords,
        description: analysis.description,
        value: analysis.value,
        value_type: analysis.value_type,
        mail_class: analysis.mail_class,
        shape: analysis.shape,
        full_bleed: analysis.full_bleed,
    };

    Ok((enrichment, usage))
}

/// Cached client for fetching images
pub struct EnrichmentClient {
    client: reqwest::blocking::Client,
    cache_dir: PathBuf,
}

impl EnrichmentClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; USPSStampEnricher/1.0)")
            .build()?;
        let cache_dir = PathBuf::from("cache");
        Ok(Self { client, cache_dir })
    }

    fn url_to_cache_path(&self, url: &str) -> PathBuf {
        let url = url.split('?').next().unwrap_or(url);
        if let Some(stripped) = url.strip_prefix("https://") {
            self.cache_dir.join(stripped)
        } else if let Some(stripped) = url.strip_prefix("http://") {
            self.cache_dir.join(stripped)
        } else {
            self.cache_dir.join(url)
        }
    }

    pub fn fetch_binary(&self, url: &str) -> Result<Vec<u8>> {
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

/// Represents an image to be processed with its year context
struct ImageToProcessWithYear {
    image: ImageToProcess,
    year: String,
    image_url: String,
    api_slug: String,
}

/// Get image info for a stamp slug, returns None if should skip
fn get_stamp_image_info(
    client: &EnrichmentClient,
    slug: &str,
    force: bool,
    quiet: bool,
) -> Result<Option<ImageToProcessWithYear>> {
    let cache_path = PathBuf::from("cache/admin.stampsforever.com/api/stamp-issuances").join(slug);

    if !cache_path.exists() {
        if !quiet {
            eprintln!("  Cache not found for {}, skipping", slug);
        }
        return Ok(None);
    }

    let json_content = fs::read_to_string(&cache_path)?;
    let stamp_data: serde_json::Value = serde_json::from_str(&json_content)?;

    // Extract year from stamp data
    let year = stamp_data["issue_year"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let images = stamp_data["images"].as_array();
    let first_image = images
        .and_then(|arr| arr.first())
        .and_then(|img| img["path"].as_str());

    let Some(image_url) = first_image else {
        if !quiet {
            eprintln!("  No stamp images found for {}", slug);
        }
        return Ok(None);
    };

    let clean_url = image_url.split('?').next().unwrap_or(image_url);
    let image_filename = clean_url
        .rsplit('/')
        .next()
        .unwrap_or("image.png")
        .to_string();

    // Check if enrichment already exists (unless force) - now in year subdirectory
    if !force {
        let base_filename = image_filename
            .trim_end_matches(".png")
            .trim_end_matches(".jpg");
        let enrichment_path = PathBuf::from(ENRICHMENT_DIR)
            .join(&year)
            .join(format!("{}.json", base_filename));
        if enrichment_path.exists() {
            if !quiet {
                let image_link = osc8_link(clean_url, &image_filename);
                let json_name = format!("{}/{}.json", year, base_filename);
                let json_link = osc8_link(&file_url(&enrichment_path), &json_name);
                println!("  Skipped: {} -> {}", image_link, json_link);
            }
            return Ok(None);
        }
    }

    // Fetch the image
    let image_data = client.fetch_binary(clean_url)?;

    Ok(Some(ImageToProcessWithYear {
        image: ImageToProcess {
            image_filename,
            image_data,
        },
        year,
        image_url: clean_url.to_string(),
        api_slug: slug.to_string(),
    }))
}

/// Create an OSC8 hyperlink for terminal output
fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

/// Create a file:// URL for a path
fn file_url(path: &PathBuf) -> String {
    let abs_path = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    format!("file://{}", abs_path.display())
}

/// Print cost summary table
fn print_summary(usage: &UsageStats, pricing: &ModelPricing) {
    let input_cost =
        (usage.prompt_tokens as f64 - usage.cached_tokens as f64) * pricing.input_cost_per_token;
    let cache_cost = usage.cached_tokens as f64 * pricing.cache_read_cost_per_token;
    let output_cost = usage.output_tokens as f64 * pricing.output_cost_per_token;
    let total_cost = input_cost + cache_cost + output_cost;

    println!();
    println!("┌──────────┬──────────────┬──────────────┬──────────────┐");
    println!("│          │       Tokens │   Cost/1M tk │         Cost │");
    println!("├──────────┼──────────────┼──────────────┼──────────────┤");
    println!(
        "│ Input    │ {:>12} │      ${:.4} │      ${:.4} │",
        usage.prompt_tokens - usage.cached_tokens,
        pricing.input_cost_per_token * 1_000_000.0,
        input_cost
    );
    println!(
        "│ Cached   │ {:>12} │      ${:.4} │      ${:.4} │",
        usage.cached_tokens,
        pricing.cache_read_cost_per_token * 1_000_000.0,
        cache_cost
    );
    println!(
        "│ Output   │ {:>12} │      ${:.4} │      ${:.4} │",
        usage.output_tokens,
        pricing.output_cost_per_token * 1_000_000.0,
        output_cost
    );
    println!("├──────────┼──────────────┼──────────────┼──────────────┤");
    println!(
        "│ Total    │ {:>12} │              │      ${:.4} │",
        usage.prompt_tokens + usage.output_tokens,
        total_cost
    );
    println!("└──────────┴──────────────┴──────────────┴──────────────┘");
    println!("Model: {}", GEMINI_MODEL);
}

/// Run the enrichment command
pub fn run_enrich(filter: Option<String>, quiet: bool, force: bool) -> Result<()> {
    let api_key = get_api_key()?;
    let client = EnrichmentClient::new()?;

    // Load pricing data
    let pricing = load_pricing()?;

    // Ensure directories exist
    fs::create_dir_all(ENRICHMENT_DIR)?;
    fs::create_dir_all(LOGS_DIR)?;

    // Get list of stamps to process
    let cache_dir = PathBuf::from("cache/admin.stampsforever.com/api/stamp-issuances");
    if !cache_dir.exists() {
        bail!("Cache directory not found. Run 'stamps sync' and 'stamps scrape' first.");
    }

    let mut entries: Vec<String> = fs::read_dir(&cache_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    entries.sort();

    // Filter if specified
    let stamps: Vec<String> = match filter {
        Some(f) => {
            if f.len() == 4 && f.chars().all(|c| c.is_ascii_digit()) {
                let year_str = f.clone();
                entries
                    .into_iter()
                    .filter(|slug| {
                        let path = cache_dir.join(slug);
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                                if let Some(issue_year) = data["issue_year"].as_str() {
                                    return issue_year == year_str;
                                }
                            }
                        }
                        false
                    })
                    .collect()
            } else {
                entries.into_iter().filter(|s| s == &f).collect()
            }
        }
        None => entries,
    };

    if stamps.is_empty() {
        bail!("No stamps found matching filter");
    }

    let total = stamps.len();
    if !quiet {
        println!(
            "Enriching {} stamps with Gemini AI analysis ({} parallel requests)...",
            total, PARALLEL_REQUESTS
        );
        if force {
            println!("Force mode enabled - regenerating all enrichment data");
        }
    }

    let mut total_usage = UsageStats::default();
    let mut processed = 0;
    let mut skipped = 0;
    let mut errors = 0;

    // Collect images to process (with year info)
    let mut images_to_process: Vec<ImageToProcessWithYear> = Vec::new();

    for (i, slug) in stamps.iter().enumerate() {
        if !quiet {
            print!("\r[{}/{}] Collecting {}...", i + 1, total, slug);
            io::stdout().flush()?;
        }

        match get_stamp_image_info(&client, slug, force, quiet) {
            Ok(Some(img_with_year)) => {
                images_to_process.push(img_with_year);
            }
            Ok(None) => {
                skipped += 1;
            }
            Err(e) => {
                errors += 1;
                if !quiet {
                    eprintln!("\n  Error: {}", e);
                }
            }
        }
    }

    if !quiet {
        println!(
            "\nCollected {} images to process, {} skipped, {} errors",
            images_to_process.len(),
            skipped,
            errors
        );
    }

    // Process images in parallel (PARALLEL_REQUESTS at a time, single image per request)
    let chunks: Vec<_> = images_to_process.chunks(PARALLEL_REQUESTS).collect();
    let total_images = images_to_process.len();

    for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
        if !quiet {
            println!(
                "\nProcessing {}-{} of {} ({} parallel requests)...",
                chunk_idx * PARALLEL_REQUESTS + 1,
                (chunk_idx * PARALLEL_REQUESTS + chunk.len()).min(total_images),
                total_images,
                chunk.len()
            );
        }

        // Spawn parallel threads for each image in the chunk
        let handles: Vec<_> = chunk
            .iter()
            .map(|img_with_year| {
                let api_key = api_key.clone();
                let image = img_with_year.image.clone();
                let year = img_with_year.year.clone();
                let image_url = img_with_year.image_url.clone();
                let api_slug = img_with_year.api_slug.clone();

                std::thread::spawn(move || {
                    let thread_client = reqwest::blocking::Client::builder()
                        .user_agent("Mozilla/5.0 (compatible; USPSStampEnricher/1.0)")
                        .build()
                        .ok()?;

                    let result = analyze_single_stamp(&thread_client, &api_key, &image);
                    Some((result, year, image.image_filename.clone(), image_url, api_slug))
                })
            })
            .collect();

        // Collect results
        for handle in handles {
            match handle.join() {
                Ok(Some((Ok((enrichment, usage)), year, _filename, image_url, api_slug))) => {
                    total_usage.add(&usage);

                    let output_filename = enrichment
                        .image_filename
                        .trim_end_matches(".png")
                        .trim_end_matches(".jpg")
                        .trim_end_matches(".jpeg");

                    // Create year/api_slug directory and save there
                    let year_dir = PathBuf::from(ENRICHMENT_DIR).join(&year).join(&api_slug);
                    fs::create_dir_all(&year_dir)?;
                    let output_path = year_dir.join(format!("{}.json", output_filename));
                    write_json_file(&output_path, &enrichment)?;

                    processed += 1;

                    if !quiet {
                        let image_link = osc8_link(&image_url, &enrichment.image_filename);
                        let json_name = format!("{}/{}/{}.json", year, api_slug, output_filename);
                        let json_link = osc8_link(&file_url(&output_path), &json_name);
                        println!("  Saved: {} -> {}", image_link, json_link);
                    }
                }
                Ok(Some((Err(e), _year, filename, image_url, _api_slug))) => {
                    errors += 1;
                    if !quiet {
                        let image_link = osc8_link(&image_url, &filename);
                        eprintln!("  Error: {} - {}", image_link, e);
                    }
                }
                Ok(None) => {
                    errors += 1;
                    if !quiet {
                        eprintln!("  Error: Failed to create HTTP client");
                    }
                }
                Err(_) => {
                    errors += 1;
                    if !quiet {
                        eprintln!("  Error: Thread panicked");
                    }
                }
            }
        }
    }

    if !quiet {
        println!(
            "\nDone! Processed: {}, Skipped: {}, Errors: {}",
            processed, skipped, errors
        );
        print_summary(&total_usage, &pricing);
    }

    Ok(())
}
