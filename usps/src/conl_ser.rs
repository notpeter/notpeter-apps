//! CONL serialization for stamp metadata
//!
//! This module provides serialization of Rust structs to CONL format.
//! CONL is a post-modern configuration language similar to YAML but simpler.

use crate::types::StampMetadata;

/// Trait for types that can be serialized to CONL
pub trait ToConl {
    fn to_conl(&self) -> String;
}

/// Escape a string value if needed for CONL
fn escape_value(s: &str) -> String {
    // Values that need quoting: start/end with space, contain = or ;, or newlines
    if s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with('"')
        || s.contains(';')
        || s.contains('=')
        || s.contains('\n')
        || s.contains('\r')
    {
        // Use quoted format with escapes
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

/// Format a multiline string for CONL
fn format_multiline(s: &str, hint: Option<&str>) -> String {
    let hint_str = hint.unwrap_or("");
    let mut result = format!("\"\"\"{}\n", hint_str);
    for line in s.lines() {
        result.push_str("  ");
        result.push_str(line);
        result.push('\n');
    }
    result
}

impl ToConl for StampMetadata {
    fn to_conl(&self) -> String {
        let mut lines = Vec::new();

        // Basic fields
        lines.push(format!("name = {}", escape_value(&self.name)));
        lines.push(format!("slug = {}", escape_value(&self.slug)));
        lines.push(format!("api_slug = {}", escape_value(&self.api_slug)));
        lines.push(format!("url = {}", escape_value(&self.url)));

        if let Some(date) = &self.issue_date {
            lines.push(format!("issue_date = {}", date));
        }
        if let Some(loc) = &self.issue_location {
            lines.push(format!("issue_location = {}", escape_value(loc)));
        }

        if let Some(rate) = self.rate {
            lines.push(format!("rate = {:.2}", rate));
        }
        if let Some(rt) = &self.rate_type {
            lines.push(format!("rate_type = {}", rt.as_str()));
        }

        lines.push(format!("forever = {}", self.forever));
        lines.push(format!("year = {}", self.year));

        // Type and series
        if self.stamp_type != crate::types::StampType::Stamp {
            lines.push(format!("type = {}", self.stamp_type.as_str()));
        }
        if let Some(series) = &self.series {
            lines.push(format!("series = {}", escape_value(series)));
        }

        if let Some(bg) = &self.background_color {
            lines.push(format!("background_color = {}", bg));
        }

        // Images
        if !self.stamp_images.is_empty() {
            lines.push("stamp_images".to_string());
            for img in &self.stamp_images {
                lines.push(format!("  = {}", img));
            }
        }
        if let Some(sheet) = &self.sheet_image {
            lines.push(format!("sheet_image = {}", sheet));
        }

        // Credits
        if !self.credits.is_empty() {
            lines.push("credits".to_string());
            if let Some(ad) = &self.credits.art_director {
                lines.push(format!("  art_director = {}", escape_value(ad)));
            }
            if let Some(a) = &self.credits.artist {
                lines.push(format!("  artist = {}", escape_value(a)));
            }
            if let Some(d) = &self.credits.designer {
                lines.push(format!("  designer = {}", escape_value(d)));
            }
            if let Some(t) = &self.credits.typographer {
                lines.push(format!("  typographer = {}", escape_value(t)));
            }
            if let Some(p) = &self.credits.photographer {
                lines.push(format!("  photographer = {}", escape_value(p)));
            }
            if let Some(i) = &self.credits.illustrator {
                lines.push(format!("  illustrator = {}", escape_value(i)));
            }
        }

        // About (multiline)
        if let Some(about) = &self.about {
            lines.push(format!("about = {}", format_multiline(about, Some("md"))));
        }

        // Products
        if !self.products.is_empty() {
            lines.push("products".to_string());
            for product in &self.products {
                lines.push("  =".to_string());
                lines.push(format!("    title = {}", escape_value(&product.title)));
                if let Some(lt) = &product.long_title {
                    lines.push(format!("    long_title = {}", escape_value(lt)));
                }
                if let Some(price) = &product.price {
                    lines.push(format!("    price = {}", escape_value(price)));
                }
                if let Some(url) = &product.postal_store_url {
                    lines.push(format!("    postal_store_url = {}", url));
                }
                if let Some(url) = &product.stamps_forever_url {
                    lines.push(format!("    stamps_forever_url = {}", url));
                }
                if !product.images.is_empty() {
                    lines.push("    images".to_string());
                    for img in &product.images {
                        lines.push(format!("      = {}", img));
                    }
                }
            }
        }

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_value() {
        assert_eq!(escape_value("hello"), "hello");
        assert_eq!(escape_value("hello world"), "hello world");
        assert_eq!(escape_value(" leading"), "\" leading\"");
        assert_eq!(escape_value("trailing "), "\"trailing \"");
        assert_eq!(escape_value("has;semicolon"), "\"has;semicolon\"");
        assert_eq!(escape_value("has=equals"), "\"has=equals\"");
        assert_eq!(escape_value("line\nbreak"), "\"line\\nbreak\"");
    }
}
