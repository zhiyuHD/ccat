use std::io::Write;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;

/// Show audio/video metadata.
pub fn cat_media(data: &[u8]) {
    let probe = match Probe::new(std::io::Cursor::new(data)).guess_file_type() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ccat: media metadata error: {e}");
            return;
        }
    };
    let tagged_file = match probe.read() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ccat: media metadata error: {e}");
            return;
        }
    };

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    let props = tagged_file.properties();
    let duration = props.duration();
    if duration.as_secs() > 0 {
        let mins = duration.as_secs() / 60;
        let secs = duration.as_secs() % 60;
        let _ = writeln!(handle, "Duration:  {}:{:02}", mins, secs);
    }
    if let Some(br) = props.overall_bitrate() {
        let _ = writeln!(handle, "Bitrate:   {} kbps", br);
    }
    let channels = props.channels().unwrap_or(0);
    let _ = writeln!(handle, "Channels:  {}", channels);
    if let Some(sr) = props.sample_rate() {
        let _ = writeln!(handle, "Sample:    {} Hz", sr);
    }

    for tag in tagged_file.tags() {
        let tag_type = format!("{:?}", tag.tag_type());
        if let Some(title) = tag.title() {
            let _ = writeln!(handle, "[{}] Title:   {}", tag_type, title);
        }
        if let Some(artist) = tag.artist() {
            let _ = writeln!(handle, "[{}] Artist:  {}", tag_type, artist);
        }
        if let Some(album) = tag.album() {
            let _ = writeln!(handle, "[{}] Album:   {}", tag_type, album);
        }
        if let Some(genre) = tag.genre() {
            let _ = writeln!(handle, "[{}] Genre:   {}", tag_type, genre);
        }
    }
}
