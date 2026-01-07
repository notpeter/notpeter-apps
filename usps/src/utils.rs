use std::fs;

/// Create an OSC8 hyperlink for terminal output
pub fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

/// Create an OSC8 file:// hyperlink for terminal output
pub fn osc8_file_link(path: &str, text: &str) -> String {
    let abs_path = fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());
    format!("\x1b]8;;file://{}\x1b\\{}\x1b]8;;\x1b\\", abs_path, text)
}
