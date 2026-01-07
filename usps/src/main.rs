use anyhow::Result;
use clap::{Parser, Subcommand};
use rusqlite::Connection;

mod enrichment;
mod generate;
mod scrape;
mod simple;
mod sync;
mod utils;

pub const STAMPS_API_URL: &str = "https://admin.stampsforever.com/api/stamp-issuances";
pub const MIN_SCRAPE_YEAR: u32 = 1996;

/// Parse date string like "June 17, 2025" to ISO 8601 "2025-06-17"
/// Returns None for TBA dates, panics on invalid date format
pub fn parse_date_to_iso(date_str: &str) -> Option<String> {
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
    /// Enrich stamps with AI image analysis (uses Gemini API)
    Enrich {
        /// Specific stamp slug or year (e.g., "love-2026" or "2025")
        #[arg(value_name = "SLUG_OR_YEAR")]
        filter: Option<String>,
        /// Quiet mode - suppress progress output
        #[arg(short, long)]
        quiet: bool,
    },
}

/// Detect stamp type based on name
/// Returns "card" for stamped cards, "envelope" for stamped envelopes, "stamp" otherwise
pub fn detect_stamp_type(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("stamped card") || lower.contains("postal card") {
        "card"
    } else if lower.contains("stamped envelope") || lower.contains("postal envelope") {
        "envelope"
    } else {
        "stamp"
    }
}

pub fn init_database(conn: &Connection) -> Result<()> {
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Simple => simple::run_simple(),
        Commands::Stamps { action } => match action {
            StampsAction::Sync { output } => sync::run_sync(&output),
            StampsAction::ScrapeDetails { filter, quiet } => scrape::run_scrape_details(filter, quiet),
            StampsAction::Generate => generate::run_generate(),
            StampsAction::Enrich { filter, quiet } => enrichment::run_enrich(filter, quiet),
        },
    }
}
