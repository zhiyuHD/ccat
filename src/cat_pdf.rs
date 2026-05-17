use std::io::Write;

use lopdf::content::{Content, Operation};

/// Extract text from PDF files by manually walking content operations.
/// Handles Tj, TJ, ' (quote), and " (double quote) operators.
pub fn cat_pdf(data: &[u8]) {
    match lopdf::Document::load_mem(data) {
        Ok(doc) => {
            let pages = doc.get_pages();
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();

            for (_, &page_id) in pages.iter() {
                match doc.get_page_content(page_id) {
                    Ok(content_data) => {
                        match Content::decode(&content_data) {
                            Ok(content) => {
                                let text = extract_text_from_ops(&content);
                                let _ = write!(handle, "{}", text);
                            }
                            Err(e) => {
                                let _ = writeln!(handle, "ccat: PDF content decode error: {e}");
                            }
                        }
                    }
                    Err(_) => continue, // page has no content
                }
            }
        }
        Err(e) => {
            eprintln!("ccat: PDF parse error: {e}");
        }
    }
}

fn extract_text_from_ops(content: &Content) -> String {
    let mut out = String::new();
    let mut i = 0;
    let ops = &content.operations;

    while i < ops.len() {
        match ops[i].operator.as_str() {
            "Tj" => {
                // (text) Tj
                if let Some(text) = extract_string(&ops[i]) {
                    out.push_str(&text);
                }
            }
            "'" => {
                // (text) '  — move to next line + show text
                out.push('\n');
                if let Some(text) = extract_string(&ops[i]) {
                    out.push_str(&text);
                }
            }
            "\"" => {
                // (text) "  — set spacing + move to next line + show text
                out.push('\n');
                if let Some(text) = extract_string(&ops[i]) {
                    out.push_str(&text);
                }
            }
            "TJ" => {
                // [(text) kern (text) ...] TJ
                for obj in &ops[i].operands {
                    if let lopdf::Object::Array(arr) = obj {
                        for item in arr {
                            if let lopdf::Object::String(bytes, _) = item {
                                let s = String::from_utf8_lossy(bytes);
                                out.push_str(&s);
                            }
                            // Numbers are kerning, skip them
                        }
                    }
                }
            }
            "T*" => {
                out.push('\n');
            }
            "ET" => {
                out.push('\n');
            }
            _ => {}
        }
        i += 1;
    }

    out
}

fn extract_string(op: &Operation) -> Option<String> {
    if let Some(obj) = op.operands.first() {
        if let lopdf::Object::String(bytes, _) = obj {
            let s = String::from_utf8_lossy(bytes).to_string();
            // Decode PDF escape sequences
            let decoded = s.replace("\\(", "(").replace("\\)", ")");
            return Some(decoded);
        }
    }
    None
}
