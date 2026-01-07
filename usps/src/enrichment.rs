use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};

const ENRICHMENT_DIR: &str = "enrichment";
const GEMINI_API_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash-lite:generateContent";

/// Stamp enrichment data from AI analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct StampEnrichment {
    /// Image filename that was analyzed
    pub image_filename: String,
    /// Year of issue shown on stamp (small text)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    /// Words/text visible on the stamp
    pub words: Vec<String>,
    /// Keywords describing the visual contents (3-7)
    pub keywords: Vec<String>,
    /// Short description of the stamp image
    pub description: String,
    /// Numeric postal value if shown (e.g., "78", "1.70") or null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    error: Option<GeminiError>,
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
    text: String,
}

#[derive(Debug, Deserialize)]
struct GeminiError {
    message: String,
}

/// Response structure we ask Gemini to return
#[derive(Debug, Deserialize)]
struct GeminiAnalysis {
    year: Option<String>,
    words: Vec<String>,
    keywords: Vec<String>,
    description: String,
    value: Option<String>,
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
        _ => "image/png", // Default
    }
}

fn analyze_stamp_image(
    client: &reqwest::blocking::Client,
    api_key: &str,
    image_data: &[u8],
    image_path: &str,
) -> Result<GeminiAnalysis> {
    let base64_image = BASE64_STANDARD.encode(image_data);
    let mime_type = get_mime_type(image_path);

    let prompt = r#"Analyze this US postage stamp image and provide the following information as a single JSON object:

{
  "year": "string or null",       // Small text year of issue shown on stamp, or null
  "words": ["string"],            // All visible text/words on the stamp (denomination, "USA", "FOREVER", etc.)
  "keywords": ["string"],         // 3-7 keywords describing visual contents
  "description": "string",        // Brief 1-2 sentence description of what the stamp depicts
  "value": "string or null",      // Numeric postal value if shown (e.g., "78", "1.70"), or null
  "value_type": "string or null", // One of: "denominated", "forever", "global forever", "postcard forever", "additional ounce", "two ounce", "three ounce", "nonmachinable", "priority mail", "priority mail express", or null
  "mail_class": "string or null", // One of: "first class", "priority mail", "priority mail express", "postcard", "presorted", "airmail", or null
  "shape": "string or null",      // One of: "portrait", "landscape", "square", "circular", "triangle"
  "full_bleed": boolean           // true if border is non-white (full bleed), false if white border
}

Respond with ONLY a single JSON object (not an array)."#;

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
        },
    };

    let url = format!("{}?key={}", GEMINI_API_URL, api_key);
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

    let candidates = gemini_response
        .candidates
        .context("No candidates in Gemini response")?;
    let first_candidate = candidates.first().context("Empty candidates array")?;
    let first_part = first_candidate
        .content
        .parts
        .first()
        .context("No parts in response content")?;

    let analysis: GeminiAnalysis = serde_json::from_str(&first_part.text)
        .with_context(|| format!("Failed to parse analysis JSON: {}", first_part.text))?;

    Ok(analysis)
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

/// Process a single stamp and save enrichment data
fn process_stamp(
    client: &EnrichmentClient,
    api_key: &str,
    slug: &str,
    quiet: bool,
) -> Result<Option<StampEnrichment>> {
    let cache_path = PathBuf::from("cache/admin.stampsforever.com/api/stamp-issuances").join(slug);

    if !cache_path.exists() {
        if !quiet {
            eprintln!("  Cache not found for {}, skipping", slug);
        }
        return Ok(None);
    }

    // Parse the cached JSON
    let json_content = fs::read_to_string(&cache_path)?;
    let stamp_data: serde_json::Value = serde_json::from_str(&json_content)?;

    // Get the first stamp image (not the pane/cover)
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

    // Extract filename from URL
    let clean_url = image_url.split('?').next().unwrap_or(image_url);
    let image_filename = clean_url
        .rsplit('/')
        .next()
        .unwrap_or("image.png")
        .to_string();

    // Check if enrichment already exists
    let enrichment_path = PathBuf::from(ENRICHMENT_DIR).join(format!(
        "{}.json",
        image_filename
            .trim_end_matches(".png")
            .trim_end_matches(".jpg")
    ));
    if enrichment_path.exists() {
        if !quiet {
            print!(".");
            io::stdout().flush()?;
        }
        return Ok(None); // Skip already processed
    }

    // Fetch the image
    let image_data = client.fetch_binary(clean_url)?;

    // Analyze with Gemini
    let analysis = analyze_stamp_image(&client.client, api_key, &image_data, &image_filename)?;

    let enrichment = StampEnrichment {
        image_filename: image_filename.clone(),
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

    Ok(Some(enrichment))
}

/// Run the enrichment command
pub fn run_enrich(filter: Option<String>, quiet: bool) -> Result<()> {
    let api_key = get_api_key()?;
    let client = EnrichmentClient::new()?;

    // Ensure enrichment directory exists
    fs::create_dir_all(ENRICHMENT_DIR)?;

    // Get list of stamps to process
    let cache_dir = PathBuf::from("cache/admin.stampsforever.com/api/stamp-issuances");
    if !cache_dir.exists() {
        bail!("Cache directory not found. Run 'stamps sync' and 'stamps scrape-details' first.");
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
                // Filter by year - need to check the JSON content
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
                // Single slug
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
        println!("Enriching {} stamps with Gemini AI analysis...", total);
    }

    let mut processed = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for (i, slug) in stamps.iter().enumerate() {
        if !quiet {
            print!("\r[{}/{}] Processing {}...", i + 1, total, slug);
            io::stdout().flush()?;
        }

        match process_stamp(&client, &api_key, slug, quiet) {
            Ok(Some(enrichment)) => {
                // Save to file
                let output_filename = enrichment
                    .image_filename
                    .trim_end_matches(".png")
                    .trim_end_matches(".jpg")
                    .trim_end_matches(".jpeg");
                let output_path =
                    PathBuf::from(ENRICHMENT_DIR).join(format!("{}.json", output_filename));
                let json = serde_json::to_string_pretty(&enrichment)?;
                fs::write(&output_path, json)?;
                processed += 1;

                if !quiet {
                    print!(" OK");
                    io::stdout().flush()?;
                }
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

        if !quiet {
            println!();
        }
    }

    if !quiet {
        println!(
            "\nDone! Processed: {}, Skipped: {}, Errors: {}",
            processed, skipped, errors
        );
    }

    Ok(())
}
