# ccat-rs

> **The Terminal Swiss Army Knife** — one binary that replaces a dozen Unix tools.

**ccat** is an enhanced `cat` that auto-detects file types and renders them beautifully in the terminal. But it doesn't stop there — it's also a full Linux system diagnostics suite, code analysis toolkit, charting engine, QR code generator, and more. All in a single ~6MB Rust binary with zero runtime dependencies.

```bash
# File viewing
ccat README.md              # Markdown with syntax highlighting
ccat report.docx            # Word document text extraction
ccat photo.png              # Image (Kitty/Sixel/half-block)
ccat data.csv               # Formatted CSV with alignment

# System diagnostics
ccat --health               # Unified health score (0-100)
ccat --meminfo              # Full memory analysis + ZRAM
ccat --ps --ps-sort -mem    # Processes sorted by memory
ccat --oom                  # OOM score risk analysis
ccat --cpu                  # CPU topology + AVX-512 flags
ccat --netstat              # Network connections (like ss)
ccat --sched                # Scheduler policy analysis
ccat --interrupts           # IRQ/SoftIRQ per-CPU distribution

# Developer tools
ccat -D old.txt new.txt     # File diff
ccat --elf /bin/ls          # ELF binary introspection
ccat --todo ~/project/      # Codebase TODO/FIXME scanner
ccat --search pattern .     # Grep mode
ccat --git HEAD             # Git object viewer

# Creative & utility
ccat --qr "https://..."     # Generate QR code
seq 1 20 | ccat --chart     # ASCII chart from data
ccat --tree .               # Directory tree
ccat --serve 8080           # HTTP server for files
ccat --watch 2 --ps         # Auto-refresh every 2s
```

## Features

### 📂 File Viewer (auto-detect by magic bytes)
| Format | What it does |
|--------|-------------|
| **Markdown** `.md` | Rendered with heading colors, syntax-highlighted code blocks (syntect), lists, tables, task lists, inline code, strikethrough, links |
| **Word** `.docx` | Text extraction with bold/italic/underline/color formatting |
| **Images** `.png .jpg .gif .webp .bmp .tiff` | Terminal display (Kitty protocol → Sixel → half-block fallback) |
| **PDF** `.pdf` | Text extraction via lopdf |
| **Audio/Video** `.mp3 .flac .ogg .mp4 .mkv` | Metadata: codec, bitrate, sample rate, duration, tags |
| **Archives** `.zip .tar .deb .rpm` | Content listing + per-file preview + multi-format support |
| **Gzip** `.gz` | Transparent decompression (like `zcat`) |
| **JSON / YAML / TOML / CSV / Logs** | Syntax-highlighted formatted output |

### 🔬 System Diagnostics (Linux, /proc-based)
| Flag | What it does | Replaces |
|------|-------------|----------|
| `--health` | Unified health score (memory/CPU/pressure/disk/network/swap/ZRAM) | htop + free + iostat + ss |
| `--meminfo` | Full `/proc/meminfo` breakdown + bar charts + OOM% | `free -m` + `cat /proc/meminfo` |
| `--ps` | Color-coded process list, sort/filter/tree | `ps aux` |
| `--cpu` | Topology tree, cache hierarchy, grouped CPU flags | `lscpu` + `cpuid` |
| `--disk` | Mount table, usage, I/O statistics | `df` + `mount` + `iostat` |
| `--netstat` | Network connections, TCP/UDP/PID filters | `ss -tulpn` |
| `--swap` | Swap/ZRAM analysis: compression, I/O, top consumers | `swapon` + `/proc/swaps` |
| `--cgroup` | Cgroup v2: controllers, PSI pressure, memory/CPU per cgroup | `systemd-cgls` |
| `--oom` | Per-process OOM scores, cgroup kill detection, risk tiers | `chomp OOM` |
| `--fd` | File descriptors per process with type classification | `lsof` |
| `--vmmap` | Virtual memory map with per-region RSS/PSS/Swap | `cat /proc/*/maps` |
| `--interrupts` | Per-CPU IRQ/SoftIRQ distribution, balance indicators | `cat /proc/interrupts` |
| `--sched` | Policy distribution, preemption leaders, scheduler tunables | `chrt` + `sched_debug` |

### 🛠️ Developer Tools
| Flag | What it does |
|------|-------------|
| `--diff` / `-D` | File comparison with side-by-side mode |
| `--elf` | ELF binary introspection: headers, sections, segments, symbols |
| `--search` / `-g` | Regex search with context, counts, file-only modes |
| `--todo` | Codebase annotation scanner (TODO/FIXME/BUG/HACK...) with git blame |
| `--git` | Git object viewer (blobs, trees, commits, tags) |
| `--inspect` / `-i` | File metadata: type, size, entropy, hashes (MD5/SHA1/SHA256) |
| `--schema` | Infer schema from JSON/TOML/YAML/CSV |
| `--source` | Source code with 200+ themes via syntect |

### 🎨 Visual & Creative
| Flag | What it does |
|------|-------------|
| `--qr` / `-Q` | QR code generation (4 ECC levels, color inversion) |
| `--chart` | ASCII bar/line charts from data |
| `--html` | Generate HTML output for browser |
| `--serve <PORT>` | HTTP server for files |

### 🧰 Utility
| Flag | What it does |
|------|-------------|
| `-n` / `-b` | Line numbering (all / non-blank) |
| `-s` | Squeeze blank lines |
| `-e` | Sed-like substitution (`s/foo/bar/`) |
| `--tree` / `-r` | Directory tree with sizes and line counts |
| `--follow` / `-f` | Watch file for changes (like `tail -f`) |
| `--hex` / `-x` | Hex dump with ASCII sidebar |
| `--watch` / `-w` | Auto-refresh system diagnostics every N seconds |
| `--theme` | Choose from 200+ syntax highlighting themes |
| `--completions` | Generate shell completions (bash/zsh/fish) |
| `--color-scheme` | auto / dark / light |

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

Requires Rust 1.80+. Build time ~2 min.

### Shell completions
```bash
ccat --completions bash  > ~/.local/share/bash-completion/completions/ccat
ccat --completions zsh   > /usr/local/share/zsh/site-functions/_ccat
ccat --completions fish  > ~/.config/fish/completions/ccat.fish
```

## Examples

```bash
# System health check
ccat --health

# Top 10 memory-consuming processes
ccat --ps --ps-sort -mem

# Find all FIXMEs in your project
ccat --todo --todo-kind fixme ~/project/

# Compare two files side by side
ccat -D --side-by-side old.txt new.txt

# Live network connections (refresh every 3s)
ccat --watch 3 --netstat

# Generate a QR code from a URL
ccat --qr --qr-ecc H "https://github.com/zhiyuHD/ccat"

# Chart CPU load data
cat load_data.txt | ccat --chart

# Serve a directory as HTML
ccat --serve 8080 .
```

## Performance

- Binary size: ~6MB (release, stripped)
- Memory: ~10MB RSS
- Startup: instant (sub-millisecond CLI parsing)
- System data reads: direct from `/proc` — no subprocess overhead
- Written in pure Rust — no C dependencies, no runtime

## License

MIT

## Author

Zhiyu Wang · [GitHub](https://github.com/zhiyuHD/ccat)
