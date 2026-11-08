//! Client mode: photos come from a frame-web server instead of local folders (`server=` in the
//! settings). Trash, Move and Undo are sent to the server, so the photo files are only ever
//! changed where they live; nothing on this machine is touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

/// Longest side of the previews asked for (the server makes 640, 1600 or 2400).
const PREVIEW: u32 = 2400;

pub struct Remote {
    base: String,
    agent: ureq::Agent,
}

/// Folders a photo can be moved to, as the server sees them.
pub struct Folders {
    pub roots: Vec<PathBuf>,
    pub all: Vec<PathBuf>,
    pub recent: Vec<PathBuf>,
}

impl Remote {
    pub fn new(base: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            // Error replies carry a JSON message worth showing, so they are read like any other.
            .http_status_as_error(false)
            .build()
            .into();
        Self { base: base.trim_end_matches('/').to_string(), agent }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let mut resp = self.agent.get(format!("{}{path}", self.base)).call().map_err(|e| e.to_string())?;
        let ok = resp.status().is_success();
        let v = read_json(&mut resp)?;
        if ok { Ok(v) } else { Err(error_text(&v)) }
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut resp = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| e.to_string())?;
        let ok = resp.status().is_success();
        let v = read_json(&mut resp)?;
        if ok { Ok(v) } else { Err(error_text(&v)) }
    }

    /// Every photo in the library, as paths on the server.
    pub fn list(&self) -> Result<Vec<PathBuf>, String> {
        let v = self.get("/api/photos")?;
        let photos = v["photos"].as_array().ok_or("unexpected reply from the server")?;
        Ok(photos.iter().filter_map(|p| p["path"].as_str()).map(PathBuf::from).collect())
    }

    /// The photo as a JPEG preview, already the right way up.
    pub fn image(&self, path: &Path) -> Result<Vec<u8>, String> {
        let url = format!("{}/img?s={PREVIEW}&p={}", self.base, encode(&path.to_string_lossy()));
        let mut resp = self.agent.get(url).call().map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(error_text(&read_json(&mut resp).unwrap_or_default()));
        }
        resp.body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())
    }

    /// Sends the photo (and its Live Photo video) to the Trash on the server. Returns its message.
    pub fn trash(&self, path: &Path) -> Result<String, String> {
        let v = self.post("/api/trash", json!({ "path": path.to_string_lossy() }))?;
        Ok(message(&v))
    }

    pub fn move_to(&self, path: &Path, dir: &Path) -> Result<String, String> {
        let v = self.post("/api/move", json!({ "path": path.to_string_lossy(), "dest": dir.to_string_lossy() }))?;
        Ok(message(&v))
    }

    /// Reverses the server's last Trash or Move (also one made from the phone).
    /// Returns its message and the photo's path now.
    pub fn undo(&self) -> Result<(String, PathBuf), String> {
        let v = self.post("/api/undo", json!({}))?;
        let path = v["photo"]["path"].as_str().ok_or("unexpected reply from the server")?;
        Ok((message(&v), PathBuf::from(path)))
    }

    pub fn folders(&self) -> Result<Folders, String> {
        let v = self.get("/api/folders")?;
        let paths = |key: &str| -> Vec<PathBuf> {
            v[key].as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(PathBuf::from).collect()
        };
        Ok(Folders { roots: paths("roots"), all: paths("folders"), recent: paths("recent") })
    }
}

fn read_json(resp: &mut ureq::http::Response<ureq::Body>) -> Result<Value, String> {
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn message(v: &Value) -> String {
    v["message"].as_str().unwrap_or("Done").to_string()
}

fn error_text(v: &Value) -> String {
    v["error"].as_str().unwrap_or("the server did not answer").to_string()
}

/// Percent-encodes a query value (UTF-8 bytes; letters, digits and `-_.~/` stay as they are).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_keeps_paths_readable() {
        assert_eq!(encode("/home/a b/收藏/x&y.jpg"), "/home/a%20b/%E6%94%B6%E8%97%8F/x%26y.jpg");
    }
}
