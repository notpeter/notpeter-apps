use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Deserialize;

use crate::{detect_stamp_type, init_database, parse_date_to_iso, STAMPS_API_URL};

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
