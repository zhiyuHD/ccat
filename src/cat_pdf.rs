use std::io::Write;

/// Extract text from PDF files.
pub fn cat_pdf(data: &[u8]) {
    match pdf_extract::extract_text_from_mem(data) {
        Ok(text) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let _ = writeln!(handle, "{}", text);
        }
        Err(e) => {
            eprintln!("ccat: PDF extraction error: {e}");
        }
    }
}
