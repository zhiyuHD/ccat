use image::load_from_memory;

/// Display an image in the terminal.
///
/// Uses viuer which auto-detects protocol support:
/// - Kitty protocol (preferred, fastest)
/// - Sixel protocol (fallback, if enabled)
/// - Half-block characters (final fallback, works everywhere)
pub fn cat_image(data: &[u8]) {
    match load_from_memory(data) {
        Ok(img) => {
            let conf = viuer::Config {
                // Auto-detect terminal protocol
                use_kitty: true,
                // Restore cursor position after image
                restore_cursor: true,
                // Use truecolor for block fallback
                truecolor: true,
                // Set width to 80 cells if image is large, None = auto-fit
                width: None,
                height: None,
                ..Default::default()
            };
            match viuer::print(&img, &conf) {
                Ok(_) => {
                    // Move cursor below the image
                    println!();
                }
                Err(e) => {
                    // Fallback: print basic info about the image
                    eprintln!("ccat: image display failed ({e}), showing image info:");
                    eprintln!("       {}x{} pixels", img.width(), img.height());
                    eprintln!("       color type: {:?}", img.color());
                }
            }
        }
        Err(e) => {
            eprintln!("ccat: image decode error: {e}");
        }
    }
}
