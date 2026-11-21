//! The XDG desktop portal's Screenshot, for desktops whose compositor shares no capture
//! protocol with ordinary clients (GNOME). It captures the whole desktop; the first time the
//! desktop may ask whether tlum may take screenshots.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

/// The whole desktop as RGBA.
pub fn screenshot(conn: &Connection) -> Result<(u32, u32, Vec<u8>), String> {
    let token = format!("tlum{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros());
    let sender = conn.unique_name().ok_or("no bus name")?.trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(conn, "org.freedesktop.portal.Desktop", path, "org.freedesktop.portal.Request").map_err(|e| e.to_string())?;
    let mut responses = request.receive_signal("Response").map_err(|e| e.to_string())?;
    let portal = Proxy::new(conn, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Screenshot").map_err(|e| e.to_string())?;
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("interactive", Value::from(false));
    let _: zbus::zvariant::OwnedObjectPath = portal.call("Screenshot", &("", options)).map_err(|e| format!("no Screenshot portal: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(responses.next()); });
    // Long enough for a first-time permission prompt.
    let msg = rx.recv_timeout(Duration::from_secs(30)).map_err(|_| "the Screenshot portal didn't answer")?.ok_or("the portal went away")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|e| e.to_string())?;
    if code != 0 { return Err("the screenshot was refused (allow tlum to take screenshots in your desktop's privacy settings)".into()); }
    let uri: String = results.get("uri").and_then(|v| String::try_from(v.clone()).ok()).ok_or("the portal sent no image")?;
    let file = urlencoding::decode(uri.trim_start_matches("file://")).map_err(|e| e.to_string())?.into_owned();
    let bytes = std::fs::read(&file).map_err(|e| format!("{file}: {e}"))?;
    let _ = std::fs::remove_file(&file);
    crate::image::decode_png(&bytes)
}
