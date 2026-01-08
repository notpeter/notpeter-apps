use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;

use crate::{detect_stamp_type, init_database, parse_date_to_iso, MIN_SCRAPE_YEAR, STAMPS_API_URL};

const EXCLUDE_FILE: &str = "enrichment/exclude.conl";

/// Load excluded slugs from enrichment/exclude.conl
fn load_excluded_slugs() -> HashSet<String> {
    let mut excluded = HashSet::new();

    let content = match fs::read_to_string(EXCLUDE_FILE) {
        Ok(c) => c,
        Err(_) => return excluded,
    };

    for line in content.lines() {
        let line = line.trim();
        // Skip empty lines and comments
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        // Parse "slug = reason" format
        if let Some((slug, _reason)) = line.split_once('=') {
            excluded.insert(slug.trim().to_string());
        }
    }

    excluded
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

pub fn run_sync(output: &str) -> Result<()> {
    // Create/open SQLite database
    let conn = Connection::open(output)?;

    init_database(&conn)?;

    // Load excluded slugs
    let excluded_slugs = load_excluded_slugs();
    if !excluded_slugs.is_empty() {
        println!("Loaded {} excluded slugs from {}", excluded_slugs.len(), EXCLUDE_FILE);
    }

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
    let mut total_excluded = 0u32;

    for stamp in &response.data {
        // Skip explicitly excluded slugs
        if excluded_slugs.contains(&stamp.slug) {
            total_excluded += 1;
            continue;
        }

        // Parse year from issue_date (works for "June 17, 2025" and "TBA 2026")
        let year: Option<u32> = stamp.issue_date.as_ref().and_then(|d| parse_year(d));

        // Skip stamps before MIN_SCRAPE_YEAR
        if let Some(y) = year {
            if y < MIN_SCRAPE_YEAR {
                continue;
            }
        }

        // Skip excluded rate types (duck stamps, presorted)
        if let Some(ref rt) = stamp.rate_type {
            match rt.as_str() {
                "Federal Duck Stamp" | "Presorted Standard" | "Presorted First-Class" | "Nonprofit" => continue,
                _ => {}
            }
        }

        let url = format!("https://www.stampsforever.com/stamps/{}", stamp.slug);

        // Parse issue_date to ISO 8601, None for TBA dates
        let iso_date: Option<String> = stamp.issue_date.as_ref().and_then(|d| parse_date_to_iso(d));

        // Detect stamp type (stamp, card, envelope)
        let stamp_type = detect_stamp_type(&stamp.name);

        let result = conn.execute(
            "INSERT OR REPLACE INTO stampsforever_stamps (slug, name, url, rate, year, issue_date, issue_location, type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                &stamp.slug,
                &stamp.name,
                &url,
                &stamp.rate_type,
                &year,
                &iso_date,
                &stamp.issue_location,
                stamp_type,
            ),
        );

        match result {
            Ok(_) => total_inserted += 1,
            Err(e) => eprintln!("  Error inserting {}: {}", stamp.name, e),
        }
    }

    println!(
        "Done! Inserted {} stamps into {} ({} excluded by slug)",
        total_inserted, output, total_excluded
    );
    Ok(())
}
