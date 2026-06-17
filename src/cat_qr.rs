//! QR code generation from text or stdin.
//!
//! Renders QR codes in the terminal using Unicode half-block characters
//! (▀ ▄ █) for a perfectly square appearance despite the 2:1 character aspect ratio.
//!
//! Use cases:
//!   ccat --qr "https://example.com"
//!   echo "some text" | ccat --qr
//!   ccat --qr --qr-size M < wifi.txt

use std::io::{self, Read};

/// Error correction level for QR codes
#[derive(Debug, Clone, Copy)]
pub enum EcLevel {
    /// Recovers 7% of data (smallest QR, default)
    L,
    /// Recovers 15% of data
    M,
    /// Recovers 25% of data
    Q,
    /// Recovers 30% of data (largest QR)
    H,
}

impl EcLevel {
    fn to_qrcode_level(self) -> qrcode::EcLevel {
        match self {
            EcLevel::L => qrcode::EcLevel::L,
            EcLevel::M => qrcode::EcLevel::M,
            EcLevel::Q => qrcode::EcLevel::Q,
            EcLevel::H => qrcode::EcLevel::H,
        }
    }
}

impl std::str::FromStr for EcLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "L" => Ok(EcLevel::L),
            "M" => Ok(EcLevel::M),
            "Q" => Ok(EcLevel::Q),
            "H" => Ok(EcLevel::H),
            _ => Err(format!("invalid error correction level '{s}', use L, M, Q, or H")),
        }
    }
}

/// Generate a QR code from data bytes and render it to a String.
pub fn render_qr(data: &str, ecc: EcLevel, invert: bool) -> String {
    let code = match qrcode::QrCode::with_error_correction_level(data, ecc.to_qrcode_level()) {
        Ok(c) => c,
        Err(e) => return format!("ccat: QR generation failed: {e}"),
    };

    let modules = code.to_colors();
    let size = code.width() as usize;

    // Add a quiet zone (4 modules padding)
    let padded = add_quiet_zone(&modules, size, 4);
    let padded_size = size + 8; // 4 on each side

    use qrcode::types::Color;

    let mut out = String::new();

    // Render two rows of QR modules as one terminal row using half-blocks
    for y in (0..padded_size).step_by(2) {
        // Top border
        out.push_str("  ");
        out.push_str("\x1b[90m│\x1b[0m");

        for x in 0..padded_size {
            let top = padded[y * padded_size + x];
            let bottom = if y + 1 < padded_size {
                padded[(y + 1) * padded_size + x]
            } else {
                Color::Light // treat off-grid bottom as light
            };

            let ch = match (top, bottom) {
                (Color::Dark, Color::Dark) => '█',
                (Color::Dark, Color::Light) => '▀',
                (Color::Light, Color::Dark) => '▄',
                (Color::Light, Color::Light) => ' ',
            };

            if invert {
                // Invert: swap dark ↔ light
                let inverted = match ch {
                    '█' => ' ',
                    ' ' => '█',
                    '▀' => '▄',
                    '▄' => '▀',
                    _ => ch,
                };
                out.push(inverted);
            } else {
                out.push(ch);
            }
        }

        out.push_str("\x1b[90m│\x1b[0m");
        out.push('\n');
    }

    out
}

/// Add quiet zone (white border) around the QR code.
fn add_quiet_zone(
    modules: &[qrcode::types::Color],
    size: usize,
    quiet: usize,
) -> Vec<qrcode::types::Color> {
    use qrcode::types::Color;

    let new_size = size + 2 * quiet;
    let mut padded = vec![Color::Light; new_size * new_size];

    for y in 0..size {
        for x in 0..size {
            padded[(y + quiet) * new_size + (x + quiet)] = modules[y * size + x];
        }
    }

    padded
}

/// Read data from stdin or use the provided text, then print the QR code.
pub fn cat_qr(text: Option<&str>, ecc: EcLevel, invert: bool) {
    let data = match text {
        Some(s) => s.to_string(),
        None => {
            let mut buf = String::new();
            if io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
                eprintln!("ccat: --qr: no input (provide text or pipe data)");
                return;
            }
            buf.trim().to_string()
        }
    };

    // Print a header line
    let byte_count = data.len();
    eprintln!(
        "\x1b[2mccat: QR code ({} byte{}, ECC={})\x1b[0m",
        byte_count,
        if byte_count == 1 { "" } else { "s" },
        match ecc {
            EcLevel::L => "L (7%)",
            EcLevel::M => "M (15%)",
            EcLevel::Q => "Q (25%)",
            EcLevel::H => "H (30%)",
        },
    );

    let qr = render_qr(&data, ecc, invert);
    print!("{qr}");
}
