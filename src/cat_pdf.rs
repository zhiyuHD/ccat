use std::io::Write;

use lopdf::content::{Content, Operation};
use lopdf::Document;

/// Extract text from PDF files.
///
/// Strategy:
/// 1. Use lopdf's built-in extract_text() for standard PDFs
/// 2. If that returns binary/CID data, fall back to manual content op parsing
///    (handles enscript/ps2pdf style PDFs using ' operator)
/// 3. If nothing works, show a clear message
pub fn cat_pdf(data: &[u8]) {
    match Document::load_mem(data) {
        Ok(doc) => {
            let pages: Vec<u32> = doc.get_pages().keys().copied().collect();

            // Try lopdf's built-in extract_text first
            let text = doc.extract_text(&pages).ok();
            if let Some(ref t) = text {
                if let Some(clean) = readable_text(t) {
                    print_text(&clean);
                    return;
                }
            }

            // Fallback: manual content operation parsing
            let mut all_text = String::new();
            for (_, &page_id) in doc.get_pages().iter() {
                if let Ok(content_data) = doc.get_page_content(page_id) {
                    if let Ok(content) = Content::decode(&content_data) {
                        let t = extract_text_from_ops(&content);
                        all_text.push_str(&t);
                    }
                }
            }

            if let Some(clean) = readable_text(&all_text) {
                print_text(&clean);
                return;
            }

            eprintln!("ccat: PDF contains no extractable text (e.g., scanned document or CID font without CMap)");
        }
        Err(e) => {
            eprintln!("ccat: PDF parse error: {e}");
        }
    }
}

fn extract_text_from_ops(content: &Content) -> String {
    let mut out = String::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "Tj" => {
                if let Some(text) = extract_string(op) {
                    out.push_str(&text);
                }
            }
            "'" => {
                out.push('\n');
                if let Some(text) = extract_string(op) {
                    out.push_str(&text);
                }
            }
            "\"" => {
                out.push('\n');
                if let Some(text) = extract_string(op) {
                    out.push_str(&text);
                }
            }
            "TJ" => {
                for obj in &op.operands {
                    if let lopdf::Object::Array(arr) = obj {
                        for item in arr {
                            if let lopdf::Object::String(bytes, _) = item {
                                out.push_str(&String::from_utf8_lossy(bytes));
                            }
                        }
                    }
                }
            }
            "T*" | "ET" => {
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

fn extract_string(op: &Operation) -> Option<String> {
    if let Some(lopdf::Object::String(bytes, _)) = op.operands.first() {
        Some(String::from_utf8_lossy(bytes).to_string())
    } else {
        None
    }
}

/// Check if text is readable (not CID-encoded binary garbage).
/// Returns Some(cleaned_text) or None if unreadable.
fn readable_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Count null bytes and non-printable chars
    let null_bytes = text.bytes().filter(|&b| b == 0).count();
    let non_printable = text.chars()
        .filter(|&c| !c.is_ascii() || (c as u8) < 0x20 && c != '\n' && c != '\r' && c != '\t')
        .count();
    let total = text.len().max(1);

    // If there's any null byte or too many non-printable chars, it's unreadable
    if null_bytes > 0 || non_printable > total / 5 {
        return None;
    }

    // Filter to printable ASCII and common whitespace
    let clean: String = text.chars()
        .filter(|&c| c.is_ascii_graphic() || c == ' ' || c == '\n' || c == '\t')
        .collect();

    let clean = clean.trim();
    if clean.is_empty() || clean.split_whitespace().count() < 2 {
        return None;
    }

    Some(clean.to_string())
}

fn print_text(text: &str) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{}", format_pdf_text(text));
}

/// Basic formatting: trim excessive whitespace, separate paragraphs.
fn format_pdf_text(text: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_blank { out.push('\n'); }
            prev_blank = true;
        } else {
            if prev_blank && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(trimmed);
            out.push('\n');
            prev_blank = false;
        }
    }

    out.trim().to_string()
}
