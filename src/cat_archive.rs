use std::io::Write;

/// List contents of archive files (zip, tar, tar.gz).
pub fn cat_archive(data: &[u8], path: &str) {
    if path.ends_with(".zip") || path.ends_with(".ZIP") {
        list_zip(data);
    } else if path.contains(".tar") {
        list_tar(data);
    } else {
        eprintln!("ccat: unknown archive format: {path}");
    }
}

fn list_zip(data: &[u8]) {
    let mut stdout = std::io::stdout();
    match zip::ZipArchive::new(std::io::Cursor::new(data)) {
        Ok(mut archive) => {
            let _ = writeln!(stdout, "Archive:  zip");
            let _ = writeln!(stdout, "  Length      Date    Time    Name");
            let _ = writeln!(stdout, "---------  ---------- -----   ----");
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index(i) {
                    let name = file.name();
                    let size = file.size();
                    let _ = writeln!(stdout, "{:>9}                     {}", size, name);
                }
            }
            let _ = writeln!(stdout, "---------                     ----");
            let _ = writeln!(stdout, "{:>9}                     {} files", archive.len(), archive.len());
        }
        Err(e) => {
            eprintln!("ccat: zip error: {e}");
        }
    }
}

fn list_tar(data: &[u8]) {
    use std::io::Read;
    let mut stdout = std::io::stdout();

    // Handle .tar.gz
    let reader: Box<dyn Read> = if data.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(data))
    } else {
        Box::new(data)
    };

    match tar::Archive::new(reader).entries() {
        Ok(entries) => {
            let _ = writeln!(stdout, "Archive:  tar");
            let _ = writeln!(stdout, "    Size    Name");
            for entry in entries {
                if let Ok(entry) = entry {
                    let size = entry.size();
                    let path = entry.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let _ = writeln!(stdout, "{:>8}    {}", size, path);
                }
            }
        }
        Err(e) => {
            eprintln!("ccat: tar error: {e}");
        }
    }
}
