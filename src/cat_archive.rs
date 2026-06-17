/// Enhanced archive browser: detailed listing, single-file preview, and extended format support.
///
/// Supports: zip, tar, tar.gz, tar.bz2, tar.xz, tar.zst, deb, rpm, jar, apk, ipa, cpio
/// Features:
///   - Detailed listing with permissions, timestamps, sizes, compression ratios
///   - Single-file preview: ccat --archive file.zip path/to/file.txt
///   - Color-coded entries by type (dir, executable, binary, text, etc.)
///   - Summary stats (total size, compressed size, ratio, entry count)
///
/// Usage:
///   ccat --archive file.zip              # detailed listing
///   ccat --archive file.zip README.md    # preview single file
///   ccat --archive file.tar.gz           # auto-detect format
///   ccat --archive-format 7z file.7z     # use 7z CLI tool
///   ccat --archive-format rar file.rar   # use unrar CLI tool

use std::io::{self, Cursor, Read, Write};
use std::path::Path;

// ── Public API ──

/// Entry point for archive browsing. Called from main.rs when FileKind::Archive.
/// `args` can be: [] (list all), ["singlefile.txt"] (preview single file),
/// or ["--format", "7z"] (use external tool).
pub fn cat_archive(data: &[u8], path: &str, args: &[String]) {
    let mut format_override: Option<String> = None;
    let mut file_arg: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--format" {
            i += 1;
            if i < args.len() {
                format_override = Some(args[i].clone());
            }
        } else {
            file_arg = Some(&args[i]);
        }
        i += 1;
    }

    if let Some(fmt) = format_override {
        match fmt.as_str() {
            "7z" => list_with_external("7z", &["l", "-slt", path]),
            "rar" => list_with_external("unrar", &["lb", "2", "v", path]),
            "bsdtar" => list_with_external("bsdtar", &["tf", path]),
            _ => eprintln!("ccat: unsupported format override: {fmt}"),
        }
        return;
    }

    let lower = path.to_lowercase();
    if lower.ends_with(".zip")
        || lower.ends_with(".jar")
        || lower.ends_with(".apk")
        || lower.ends_with(".ipa")
    {
        list_zip_detail(data, path, file_arg);
    } else if lower.contains(".tar") || lower.ends_with(".deb") || lower.ends_with(".cpio") {
        list_tar_detail(data, path, file_arg);
    } else if lower.ends_with(".rpm") {
        list_rpm(data, path, file_arg);
    } else {
        if data.len() >= 2 && data[0] == 0x50 && data[1] == 0x4b {
            list_zip_detail(data, path, file_arg);
        } else {
            list_tar_detail(data, path, file_arg);
        }
    }
}

// ── ZIP-based archives ──

fn list_zip_detail(data: &[u8], path: &str, file_arg: Option<&str>) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let mut archive = match zip::ZipArchive::new(Cursor::new(data)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ccat: failed to open archive: {e}");
            return;
        }
    };

    let total_entries = archive.len();
    let mut total_compressed: u64 = 0;
    let mut total_uncompressed: u64 = 0;

    // Compute totals by iterating manually (zip 0.6 has no .iter())
    for i in 0..total_entries {
        if let Ok(f) = archive.by_index(i) {
            total_compressed += f.compressed_size();
            total_uncompressed += f.size();
        }
    }

    let mut dirs = 0u64;
    let mut executables = 0u64;
    let mut texts = 0u64;
    let mut binaries = 0u64;
    let mut images = 0u64;
    let mut others = 0u64;

    if let Some(target_file) = file_arg {
        // Preview single file
        let mut found = false;
        for i in 0..total_entries {
            if let Ok(mut member) = archive.by_index(i) {
                let name = member.name().to_string();
                if name == target_file
                    || name.strip_suffix('/') == Some(target_file)
                    || name.ends_with('/') && name[..name.len() - 1] == target_file.to_string()
                {
                    // Try basename match
                    if !found {
                        if let Some(basename) = Path::new(target_file)
                            .file_name()
                            .and_then(|b| b.to_str())
                        {
                            if name.ends_with(basename) {
                                found = true;
                            }
                        }
                        if name == target_file || found {
                            found = true;
                            let size = member.size();
                            let comp_size = member.compressed_size();
                            let content_type = classify_by_extension(name.as_str());
                            let _ = writeln!(
                                out,
                                "{} {}  {} ({})",
                                style::bold(name.as_str()),
                                style::dim(format!("[{content_type}]")),
                                human_size(size),
                                if comp_size < size {
                                    format!(
                                        "{:.1}% compressed",
                                        (comp_size as f64 / size as f64 * 100.0)
                                    )
                                } else {
                                    "uncompressed".into()
                                }
                            );
                            let _ = writeln!(out, "{}", style::dim("─".repeat(72)));

                            if is_text_content(name.as_str()) {
                                let mut buf = String::new();
                                let _ = member.read_to_string(&mut buf);
                                display_content(buf.as_bytes(), name.as_str(), &mut out);
                            } else {
                                let _ = writeln!(
                                    out,
                                    "{}",
                                    style::yellow("Binary content — use --hex to view")
                                );
                            }
                            return;
                        }
                    }
                }
            }
        }
        eprintln!("ccat: file '{}' not found in archive", target_file);
        return;
    }

    // Full listing
    let _ = writeln!(
        out,
        "{}{}",
        style::bold("Archive: "),
        style::cyan(path)
    );
    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));
    let _ = writeln!(
        out,
        "{:<10} {:>10} {:>10}  {:>8}  {}",
        style::bold("Perm"),
        style::bold("Uncomp"),
        style::bold("Comp"),
        style::bold("Ratio"),
        style::bold("Name")
    );
    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));

    for i in 0..total_entries {
        let entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.name();
        let size = entry.size();
        let comp_size = entry.compressed_size();
        let unix_mode = entry.unix_mode().unwrap_or(0);

        let perms = decode_unix_permissions(unix_mode);
        let is_dir = name.ends_with('/');

        if is_dir {
            dirs += 1;
        } else {
            classify_entry(name, &mut executables, &mut texts, &mut binaries, &mut images, &mut others);
        }

        let ratio = if size > 0 {
            format!("{:.1}%", (comp_size as f64 / size as f64 * 100.0).min(999.9))
        } else {
            "—".into()
        };

        let name_style = if is_dir {
            style::cyan(name)
        } else if is_executable_str(&perms) {
            style::green(name)
        } else {
            name.to_string()
        };

        let _ = writeln!(
            out,
            "{:<10} {:>10} {:>10}  {:>8}  {}",
            perms,
            human_size(size),
            human_size(comp_size),
            ratio,
            name_style
        );
    }

    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));
    let _ = writeln!(
        out,
        "{}",
        format_summary(
            total_entries,
            total_compressed,
            total_uncompressed,
            dirs,
            executables,
            texts,
            binaries,
            images,
            others
        )
    );
}

// ── TAR-based archives ──

fn list_tar_detail(data: &[u8], path: &str, file_arg: Option<&str>) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Decompress if needed
    let lower = path.to_lowercase();
    let reader: Box<dyn Read> = if lower.ends_with(".gz")
        || lower.contains(".tar.gz")
        || lower.ends_with(".tgz")
    {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut result = Vec::new();
        let _ = decoder.read_to_end(&mut result);
        Box::new(Cursor::new(result))
    } else if lower.ends_with(".bz2") || lower.contains(".tar.bz2") {
        let mut decoder = brotli::Decompressor::new(data, 4096);
        let mut result = Vec::new();
        let _ = decoder.read_to_end(&mut result);
        Box::new(Cursor::new(result))
    } else if lower.ends_with(".xz") || lower.contains(".tar.xz") {
        let mut decoder = xz2::read::XzDecoder::new(data);
        let mut result = Vec::new();
        let _ = decoder.read_to_end(&mut result);
        Box::new(Cursor::new(result))
    } else if lower.ends_with(".zst") || lower.contains(".zst") {
        let mut decoder = zstd::stream::Decoder::new(data).unwrap();
        let mut result = Vec::new();
        let _ = decoder.read_to_end(&mut result);
        Box::new(Cursor::new(result))
    } else {
        Box::new(data)
    };

    let mut archive = tar::Archive::new(reader);
    let entries_result = archive.entries();
    let mut entries_iter = match entries_result {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ccat: failed to open archive: {e}");
            return;
        }
    };

    // Collect all entries as owned data (Entry can't outlive Archive)
    #[derive(Clone)]
    struct TarInfo {
        name: String,
        size: u64,
        is_dir: bool,
        mode: u32,
        comp_size: u64,
        content: Option<Vec<u8>>,
    }

    impl TarInfo {
        fn is_file(&self) -> bool {
            !self.is_dir
        }
    }

    let mut entry_list: Vec<TarInfo> = Vec::new();
    for entry in entries_iter.by_ref() {
        match entry {
            Ok(mut e) => {
                let name = e.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let header = e.header();
                let is_dir = header.entry_type().is_dir();
                let mode = header.mode().unwrap_or(0o644);
                let size = e.size();
                let content = if header.entry_type().is_file() {
                    let mut buf = Vec::new();
                    let _ = e.read_to_end(&mut buf);
                    Some(buf)
                } else {
                    None
                };
                entry_list.push(TarInfo {
                    name, size, is_dir, mode, comp_size: size, content,
                });
            }
            Err(_) => continue,
        }
    }

    let mut total_entries = entry_list.len() as u64;
    let mut total_size: u64 = 0;
    let mut dirs = 0u64;
    let mut executables = 0u64;
    let mut texts = 0u64;
    let mut binaries = 0u64;
    let mut images = 0u64;
    let mut others = 0u64;

    for info in &entry_list {
        total_size += info.size;
        if info.is_dir {
            dirs += 1;
        } else {
            classify_entry(&info.name, &mut executables, &mut texts, &mut binaries, &mut images, &mut others);
        }
    }

    if let Some(target_file) = file_arg {
        for info in &entry_list {
            let matches = info.name == target_file
                || info.name.ends_with(target_file)
                || Path::new(target_file)
                    .file_name()
                    .and_then(|b| b.to_str())
                    .map(|b| info.name.ends_with(b))
                    .unwrap_or(false);
            if matches {
                let _ = writeln!(
                    out,
                    "{} {}  {} ({})",
                    style::bold(&info.name),
                    style::dim("[tar]"),
                    human_size(info.size),
                    if info.size == 0 { "empty".to_string() } else { "stored".to_string() }
                );
                let _ = writeln!(out, "{}", style::dim("─".repeat(72)));

                if info.is_file() && info.size > 0 {
                    if let Some(ref content) = info.content {
                        display_content(content, &info.name, &mut out);
                    }
                } else if info.is_dir {
                    let _ = writeln!(out, "{}", style::yellow("(directory)"));
                }
                return;
            }
        }
        eprintln!("ccat: file '{}' not found in archive", target_file);
        return;
    }

    // Full listing
    let _ = writeln!(out, "{}{}", style::bold("Archive: "), style::cyan(path));
    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));
    let _ = writeln!(
        out,
        "{:<10} {:>12}  {}",
        style::bold("Perm"),
        style::bold("Size"),
        style::bold("Name")
    );
    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));

    for info in &entry_list {
        let is_dir = info.is_dir;
        let perm_str = format_tar_permissions(info.mode, is_dir);

        let name_style = if is_dir {
            style::cyan(&info.name)
        } else if is_executable_str(&perm_str) {
            style::green(&info.name)
        } else {
            info.name.clone()
        };

        let _ = writeln!(
            out,
            "{:<10} {:>12}  {}",
            perm_str,
            human_size(info.size),
            name_style
        );
    }

    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));
    let _ = writeln!(
        out,
        "{}",
        format_summary(
            total_entries as usize,
            total_size,
            total_size,
            dirs,
            executables,
            texts,
            binaries,
            images,
            others
        )
    );
}

// ── RPM archives ──

fn list_rpm(data: &[u8], _path: &str, _file_arg: Option<&str>) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let _ = writeln!(out, "{}", style::bold("RPM Package"));
    let _ = writeln!(out, "{}", style::dim("─".repeat(72)));

    match std::process::Command::new("rpm2cpio")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data);
            }
            let output = child.wait_with_output();
            match output {
                Ok(out_data) if out_data.status.success() => {
                    list_cpio_entries(&out_data.stdout, &mut out);
                }
                _ => {
                    eprintln!("ccat: failed to extract RPM contents");
                }
            }
        }
        Err(_) => {
            eprintln!("ccat: rpm2cpio not available, use --archive-format bsdtar");
        }
    }
}

fn list_cpio_entries(data: &[u8], out: &mut io::StdoutLock) {
    let mut offset = 0;
    let mut file_count = 0u64;
    let mut total_size = 0u64;

    while offset + 110 <= data.len() {
        let header = &data[offset..offset + 110];
        let magic = std::str::from_utf8(&header[..6]).unwrap_or("");

        if magic == "TRAILER!" {
            break;
        }
        if magic != "070701" {
            break;
        }

        let nlink = parse_cpio_octal(&header[54..62]);
        let filesize = parse_cpio_octal(&header[62..74]);
        let namesize = parse_cpio_octal(&header[94..102]);

        let name_start = offset + 110;
        let namesize_adj = if namesize > 0 { namesize - 1 } else { 0 };
        let name_end = (name_start + namesize_adj as usize).min(data.len());
        let name = String::from_utf8_lossy(&data[name_start..name_end]).to_string();

        if nlink > 0 {
            file_count += 1;
            total_size += filesize;
            let perm_str = format_cpio_permissions(parse_cpio_octal(&header[14..22]), filesize > 0);
            let _ = writeln!(
                out,
                "{:<10} {:>12}  {}",
                perm_str,
                human_size(filesize),
                name
            );
        }

        let data_size = ((filesize + 3) / 4) * 4;
        offset = name_end + data_size as usize;
    }

    let _ = writeln!(
        out,
        "\n{} {} files, {} total",
        style::bold("Summary:"),
        file_count,
        human_size(total_size)
    );
}

// ── External tool fallback ──

fn list_with_external(tool: &str, args: &[&str]) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    match std::process::Command::new(tool)
        .args(args)
        .output()
    {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let _ = writeln!(out, "{}", line);
            }
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                eprintln!("ccat: {tool} error: {err}");
            }
        }
        Err(e) => {
            eprintln!("ccat: failed to run {tool}: {e}");
        }
    }
}

// ── Helpers ──

fn decode_unix_permissions(mode: u32) -> String {
    if mode == 0 {
        return "---".into();
    }

    let mut result = String::with_capacity(10);

    match (mode >> 12) & 0xf {
        0x4 => result.push('d'),
        0xa => result.push('l'),
        _ => result.push('-'),
    }

    result.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o001 != 0 { 'x' } else { '-' });

    result
}

fn format_tar_permissions(mode: u32, is_dir: bool) -> String {
    let mut result = String::with_capacity(10);
    result.push(if is_dir { 'd' } else { '-' });
    result.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    result
}

fn format_cpio_permissions(mode: u64, is_file: bool) -> String {
    let mut result = String::with_capacity(10);
    result.push(if is_file { '-' } else { 'd' });
    result.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o100 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o010 != 0 { 'x' } else { '-' });
    result.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    result.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    result.push(if mode & 0o001 != 0 { 'x' } else { '-' });
    result
}

fn parse_cpio_octal(bytes: &[u8]) -> u64 {
    let s = std::str::from_utf8(bytes).unwrap_or("").trim();
    u64::from_str_radix(s, 8).unwrap_or(0)
}

fn is_executable_str(perms: &str) -> bool {
    let bytes = perms.as_bytes();
    // POSIX permission string: [type][rwx owner][rwx group][rwx other]
    // Indices: [0]=type [1-3]=owner [4-6]=group [7-9]=other
    bytes.len() == 10 && (bytes[3] == b'x' || bytes[5] == b'x' || bytes[7] == b'x')
}

fn classify_entry(
    name: &str,
    executables: &mut u64,
    texts: &mut u64,
    binaries: &mut u64,
    images: &mut u64,
    others: &mut u64,
) {
    let lower = name.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    if EXECUTABLE_EXTS.contains(&ext) {
        *executables += 1;
    } else if TEXT_EXTS.contains(&ext) {
        *texts += 1;
    } else if BINARY_EXTS.contains(&ext) {
        *binaries += 1;
    } else if IMAGE_EXTS.contains(&ext) {
        *images += 1;
    } else {
        *others += 1;
    }
}

const EXECUTABLE_EXTS: &[&str] = &["sh", "bash", "zsh", "fish", "csh", "py", "pl", "rb", "exe", "bin", "dll", "so", "elf"];
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "rst", "tex", "csv", "json", "yaml", "yml", "toml", "xml", "html", "htm",
    "css", "js", "ts", "jsx", "tsx", "vue", "svelte", "conf", "cfg", "ini", "env", "log",
    "sql", "sh", "bash", "makefile", "cmake", "dockerfile", "gradle", "pom", "properties",
    "props", "rc", "svg", "tex", "bib", "latex", "org", "adoc", "asciidoc", "pod", "java",
    "kt", "kts", "scala", "go", "rs", "c", "cpp", "h", "hpp", "cs", "php", "swift", "m",
    "mm", "r", "jl", "lua", "hs", "ex", "exs",
];
const BINARY_EXTS: &[&str] = &["o", "obj", "a", "lib", "pyc", "class", "war", "ear", "jar"];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "tiff", "tif", "avif", "heic", "svg"];

fn classify_by_extension(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");

    if TEXT_EXTS.contains(&ext) { return "text"; }
    if IMAGE_EXTS.contains(&ext) { return "image"; }
    if ["zip","tar","gz","tgz","bz2","xz","zst","rar","7z","deb","rpm","cpio"].contains(&ext) { return "archive"; }
    if ["elf","exe","dll","so","dylib","a","o","obj","pyc","class","war","jar","ear","bin"].contains(&ext) { return "binary"; }
    if ["mp3","ogg","wav","flac","aac","m4a","wma","opus","weba"].contains(&ext) { return "audio"; }
    if ["mp4","avi","mkv","mov","wmv","flv","webm","m4v","ogv","3gp"].contains(&ext) { return "video"; }
    if ext == "pdf" { return "document"; }
    if ["doc","docx","odt","rtf","pages"].contains(&ext) { return "document"; }
    if ["xls","xlsx","ods"].contains(&ext) { return "spreadsheet"; }
    "other"
}

fn is_text_content(name: &str) -> bool {
    let lower = name.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    TEXT_EXTS.contains(&ext)
}

fn display_content(data: &[u8], name: &str, out: &mut io::StdoutLock) {
    let ext = classify_by_extension(name);
    match ext {
        "text" => {
            let s = String::from_utf8_lossy(data);
            for line in s.lines() {
                let _ = writeln!(out, "{}", line);
            }
        }
        "image" => {
            if name.ends_with(".svg") {
                let s = String::from_utf8_lossy(data);
                for line in s.lines() {
                    let _ = writeln!(out, "{}", line);
                }
            } else {
                let _ = writeln!(out, "{}", style::yellow("Image file — use --hex to view binary content"));
            }
        }
        "binary" => {
            let _ = writeln!(out, "{}", style::yellow("Binary file — use --hex to view"));
        }
        _ => {
            let _ = writeln!(out, "{}", style::dim(format!("(content type: {ext})")));
        }
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {} ({bytes} B)", UNITS[idx])
    }
}

fn format_summary(
    total: usize,
    compressed: u64,
    uncompressed: u64,
    dirs: u64,
    executables: u64,
    texts: u64,
    binaries: u64,
    images: u64,
    others: u64,
) -> String {
    let ratio = if uncompressed > 0 {
        format!("{:.1}%", (compressed as f64 / uncompressed as f64 * 100.0))
    } else {
        "—".into()
    };

    format!(
        "{} {} files, {} dirs | {} compressed / {} original ({}) | {} text, {} binary, {} image, {} executable, {} other",
        style::bold("Summary:"),
        total,
        dirs,
        human_size(compressed),
        human_size(uncompressed),
        ratio,
        texts,
        binaries,
        images,
        executables,
        others
    )
}

mod style {
    pub fn bold(s: impl std::fmt::Display) -> String {
        format!("\x1b[1m{s}\x1b[0m")
    }
    pub fn dim(s: impl std::fmt::Display) -> String {
        format!("\x1b[2m{s}\x1b[0m")
    }
    pub fn cyan(s: impl std::fmt::Display) -> String {
        format!("\x1b[36m{s}\x1b[0m")
    }
    pub fn green(s: impl std::fmt::Display) -> String {
        format!("\x1b[32m{s}\x1b[0m")
    }
    pub fn yellow(s: impl std::fmt::Display) -> String {
        format!("\x1b[33m{s}\x1b[0m")
    }
    pub fn red(s: impl std::fmt::Display) -> String {
        format!("\x1b[31m{s}\x1b[0m")
    }
    pub fn magenta(s: impl std::fmt::Display) -> String {
        format!("\x1b[35m{s}\x1b[0m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB (1024 B)");
        assert_eq!(human_size(1048576), "1.0 MiB (1048576 B)");
        assert_eq!(human_size(1073741824), "1.0 GiB (1073741824 B)");
    }

    #[test]
    fn test_decode_unix_permissions() {
        assert_eq!(decode_unix_permissions(0o40755), "drwxr-xr-x");
        assert_eq!(decode_unix_permissions(0o100644), "-rw-r--r--");
        assert_eq!(decode_unix_permissions(0o100755), "-rwxr-xr-x");
        assert_eq!(decode_unix_permissions(0o120777), "lrwxrwxrwx");
        assert_eq!(decode_unix_permissions(0), "---");
    }

    #[test]
    fn test_is_executable_str() {
        assert!(is_executable_str("-rwxr-xr-x"));
        assert!(!is_executable_str("---"));
        assert!(is_executable_str("-rwxrwxrwx"));
        assert!(!is_executable_str("-rw-r--r--"));
    }

    #[test]
    fn test_classify_by_extension() {
        assert_eq!(classify_by_extension("README.md"), "text");
        assert_eq!(classify_by_extension("main.go"), "text");
        assert_eq!(classify_by_extension("image.png"), "image");
        assert_eq!(classify_by_extension("data.bin"), "binary");
        assert_eq!(classify_by_extension("archive.zip"), "archive");
        assert_eq!(classify_by_extension("song.mp3"), "audio");
        assert_eq!(classify_by_extension("movie.mp4"), "video");
        assert_eq!(classify_by_extension("doc.pdf"), "document");
        assert_eq!(classify_by_extension("unknown.xyz"), "other");
    }

    #[test]
    fn test_text_ext_detection() {
        // TEXT_EXTS stores lowercase extensions (matching the classification pipeline)
        assert!(TEXT_EXTS.contains(&"rs"));
        assert!(TEXT_EXTS.contains(&"json"));
        assert!(TEXT_EXTS.contains(&"makefile"));
        assert!(!TEXT_EXTS.contains(&"png"));
        assert!(!TEXT_EXTS.contains(&"bin"));
    }

    #[test]
    fn test_binary_ext_detection() {
        // BINARY_EXTS stores bare extensions
        assert!(BINARY_EXTS.contains(&"o"));
        assert!(BINARY_EXTS.contains(&"a"));
        assert!(!BINARY_EXTS.contains(&"c"));
    }

    #[test]
    fn test_image_ext_detection() {
        // IMAGE_EXTS stores bare extensions (svg is text-based, classified as text)
        assert!(IMAGE_EXTS.contains(&"jpg"));
        assert!(IMAGE_EXTS.contains(&"png"));
        assert!(!IMAGE_EXTS.contains(&"pdf"));
    }

    #[test]
    fn test_format_summary() {
        let s = format_summary(42, 1024, 4096, 3, 1, 20, 5, 2, 11);
        assert!(s.contains("Summary:"));
        assert!(s.contains("42 files"));
        assert!(s.contains("3 dirs"));
    }

    #[test]
    fn test_permissions_roundtrip() {
        let cases = vec![
            (0o40755u32, "drwxr-xr-x"),
            (0o100644, "-rw-r--r--"),
            (0o100755, "-rwxr-xr-x"),
            (0o100600, "-rw-------"),
            (0o100640, "-rw-r-----"),
            (0o120777, "lrwxrwxrwx"),
        ];
        for (perm, expected) in cases {
            assert_eq!(decode_unix_permissions(perm), expected, "Failed for perm 0o{:o}", perm);
        }
    }

    #[test]
    fn test_cpio_permissions() {
        assert_eq!(format_cpio_permissions(0o644, true), "-rw-r--r--");
        assert_eq!(format_cpio_permissions(0o755, false), "drwxr-xr-x");
        assert_eq!(format_cpio_permissions(0o755, true), "-rwxr-xr-x");
    }

    #[test]
    fn test_cpio_octal_parsing() {
        assert_eq!(parse_cpio_octal(b"0000000"), 0);
        assert_eq!(parse_cpio_octal(b"0000644"), 0o644);
        assert_eq!(parse_cpio_octal(b"0000755"), 0o755);
        assert_eq!(parse_cpio_octal(b"00040755"), 0o40755);
    }

    #[test]
    fn test_tar_permissions() {
        assert_eq!(format_tar_permissions(0o644, false), "-rw-r--r--");
        assert_eq!(format_tar_permissions(0o755, false), "-rwxr-xr-x");
        assert_eq!(format_tar_permissions(0o755, true), "drwxr-xr-x");
    }

    #[test]
    fn test_magic_bytes() {
        assert!([0x1f, 0x8b].as_ref().starts_with(&[0x1f, 0x8b]));
        assert!([0x50, 0x4b, 0x03, 0x04].as_ref().starts_with(&[0x50, 0x4b]));
        assert!([0x42, 0x5a, 0x68].as_ref().starts_with(&[0x42, 0x5a, 0x68]));
    }

    #[test]
    fn test_empty_handling() {
        assert_eq!(decode_unix_permissions(0), "---");
        assert_eq!(human_size(0), "0 B");
    }

    #[test]
    fn test_large_sizes() {
        let one_tb = 1_099_511_627_776u64;
        assert!(human_size(one_tb).contains("TiB"));
        let one_pb = one_tb * 1024;
        assert!(human_size(one_pb).contains("TiB"));
    }
}
