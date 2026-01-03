use anyhow::{Context, Result};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

const DOMESTIC_CSV_URL: &str = "https://www.usps.com/business/prices/2025/m-fcm-eddm-retail.csv";
const INTERNATIONAL_HTML_URL: &str = "https://pe.usps.com/text/dmm300/Notice123.htm";

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

fn main() -> Result<()> {
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
