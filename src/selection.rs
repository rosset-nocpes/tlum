//! The primary selection (the text last selected anywhere), read through the compositor's
//! data-control protocol, as clipboard managers do: macOS looks up the selection when the
//! force click lands inside it, and `scan::with_selection` decides whether it does.

use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

/// The selected text, if there is a short one; never blocks for long.
pub fn primary() -> Option<String> {
    use wl_clipboard_rs::paste::{ClipboardType, MimeType, Seat, get_contents};
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let got = get_contents(ClipboardType::Primary, Seat::Unspecified, MimeType::Text).ok().and_then(|(pipe, _)| {
            let mut buf = Vec::new();
            pipe.take(2048).read_to_end(&mut buf).ok()?;
            String::from_utf8(buf).ok()
        });
        let _ = tx.send(got);
    });
    rx.recv_timeout(Duration::from_millis(250)).ok().flatten().filter(|s| !s.trim().is_empty())
}
