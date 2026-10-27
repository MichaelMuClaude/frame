//! Small persistent settings file (`~/.config/frame/settings.txt`, `%APPDATA%\frame\settings.txt` on Windows).
//! Plain `key=value` lines; `dir` and `recent` may repeat.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    /// Folders the photos are picked from.
    pub dirs: Vec<PathBuf>,
    /// Folders photos were recently moved to, most recent first.
    pub recent: Vec<PathBuf>,
    pub interval: u64,
    pub fit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { dirs: Vec::new(), recent: Vec::new(), interval: 10, fit: true }
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

pub fn file_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| home_dir().map(|h| h.join(".config")))?;
    Some(base.join("frame").join("settings.txt"))
}

pub fn load() -> Settings {
    let mut st = Settings::default();
    let Some(text) = file_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return st };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "dir" => st.dirs.push(PathBuf::from(value)),
            "recent" => st.recent.push(PathBuf::from(value)),
            "interval" => {
                if let Ok(n) = value.trim().parse::<u64>() {
                    st.interval = n.clamp(1, 3600);
                }
            }
            "fit" => st.fit = value.trim() != "0",
            _ => {}
        }
    }
    st
}

pub fn save(st: &Settings) {
    let Some(path) = file_path() else { return };
    let mut out = String::new();
    for d in &st.dirs {
        out.push_str(&format!("dir={}\n", d.display()));
    }
    for d in &st.recent {
        out.push_str(&format!("recent={}\n", d.display()));
    }
    out.push_str(&format!("interval={}\nfit={}\n", st.interval, u8::from(st.fit)));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("could not save settings to {}: {e}", path.display());
    }
}
