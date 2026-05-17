# ccat-rs

**Enhanced `cat` — auto-detect and render markdown, Word documents, images, and gzip files.**

```bash
# Plain text / code (same as cat)
ccat foo.txt

# Markdown with syntax highlighting
ccat README.md

# Word document
ccat report.docx

# Image (Kitty protocol / Sixel / half-block fallback)
ccat photo.png

# Gzip compressed file
ccat data.txt.gz

# Pipe support
curl -s https://example.com/doc.md | ccat

# Multiple files
ccat intro.md body.md outro.md
```

## Installation

### Arch Linux (AUR)

```bash
yay -S ccat-rs
# or
paru -S ccat-rs
```

### From source

```bash
git clone https://github.com/zhiyuHD/ccat.git
cd ccat
cargo build --release
cp target/release/ccat ~/.local/bin/
```

## Options

| Flag | Description |
|------|-------------|
| `-A`, `--ascii` | Plain text only (skip markdown/docx/image processing) |
| `-B`, `--binary` | Raw output, no processing |
| `-T`, `--type`  | Show detected file type on stderr |

## Features

- **Markdown** — rendered with heading colors, syntax-highlighted code blocks (via syntect), lists, blockquotes, tables, task lists, inline code, strikethrough, links
- **Word (.docx)** — extracts text with bold, italic, underline, strikethrough, and color formatting
- **Images** — auto-detects Kitty terminal protocol, Sixel, and falls back to half-block character rendering (via viuer)
- **Gzip** — transparent decompression (like `zcat`)
- **Auto-detection** — identifies file type by magic bytes, extension, or markdown heuristics

## License

MIT
