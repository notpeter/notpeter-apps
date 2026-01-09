use anyhow::Result;
use clap::{Parser, Subcommand};
use rusqlite::Connection;
use std::fs;
use std::path::Path;

mod enrichment;
mod generate;
mod scrape;
mod simple;
mod sync;
mod types;
mod utils;

pub use types::*;

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
    /// Build/update the stamps SQLite database from API
    Sync {
        /// Output SQLite database file
        #[arg(short, long, default_value = "stamps.db")]
        output: String,
    },
    /// Scrape detailed stamp info, images, and metadata
    Scrape {
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
        /// Force regeneration of existing enrichment data
        #[arg(short, long)]
        force: bool,
    },
    /// Clean generated files (stamps.db and data/ folder)
    Clean,
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
    // Read and execute schema from SQL file
    let schema = include_str!("../schema.sql");
    conn.execute_batch(schema)?;
    Ok(())
}

fn run_clean() -> Result<()> {
    println!("Cleaning generated files...");

    // Remove stamps.db
    let db_path = Path::new("stamps.db");
    if db_path.exists() {
        fs::remove_file(db_path)?;
        println!("  Removed stamps.db");
    }

    // Remove data/ folder
    let data_path = Path::new("data");
    if data_path.exists() {
        fs::remove_dir_all(data_path)?;
        println!("  Removed data/");
    }

    println!("Clean complete!");
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Simple => simple::run_simple(),
        Commands::Stamps { action } => match action {
            StampsAction::Sync { output } => sync::run_sync(&output),
            StampsAction::Scrape { filter, quiet } => scrape::run_scrape(filter, quiet),
            StampsAction::Generate => generate::run_generate(),
            StampsAction::Enrich { filter, quiet, force } => {
                enrichment::run_enrich(filter, quiet, force)
            }
            StampsAction::Clean => run_clean(),
        },
    }
}
