//! Small persistent settings file (`~/.config/frame/settings.txt`, `%APPDATA%\frame\settings.txt` on Windows).
//! Plain `key=value` lines; `dir`, `exclude`, `recent`, `server_dir` and `server_exclude` may repeat.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    /// Folders the photos are picked from.
    pub dirs: Vec<PathBuf>,
    /// Folders (inside the photo folders) whose photos are never shown.
    pub exclude: Vec<PathBuf>,
    /// Folders photos were recently moved to, most recent first.
    pub recent: Vec<PathBuf>,
    pub interval: u64,
    pub fit: bool,
    /// Window position and size (x, y, width, height) when it was last moved or resized.
    pub window: Option<[f32; 4]>,
    /// A frame-web server to show photos from instead of local folders (client mode).
    pub server: Option<String>,
    /// Client mode: this frame picks its own folders on the server (`server_dirs`,
    /// `server_exclude`) instead of following the server's settings.
    pub server_own: bool,
    /// Client mode, own folders: server folders to show; empty means the whole library.
    pub server_dirs: Vec<PathBuf>,
    /// Client mode, own folders: server folders never shown here.
    pub server_exclude: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { dirs: Vec::new(), exclude: Vec::new(), recent: Vec::new(), interval: 10, fit: true, window: None, server: None,
            server_own: false, server_dirs: Vec::new(), server_exclude: Vec::new() }
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
            "exclude" => st.exclude.push(PathBuf::from(value)),
            "recent" => st.recent.push(PathBuf::from(value)),
            "interval" => {
                if let Ok(n) = value.trim().parse::<u64>() {
                    st.interval = n.clamp(1, 3600);
                }
            }
            "fit" => st.fit = value.trim() != "0",
            "server" if !value.trim().is_empty() => st.server = Some(value.trim().to_string()),
            "server_folders" => st.server_own = value.trim() == "own",
            "server_dir" => st.server_dirs.push(PathBuf::from(value)),
            "server_exclude" => st.server_exclude.push(PathBuf::from(value)),
            "window" => {
                let v: Vec<f32> = value.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                if let [x, y, w, h] = v[..] {
                    st.window = Some([x, y, w, h]);
                }
            }
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
    for d in &st.exclude {
        out.push_str(&format!("exclude={}\n", d.display()));
    }
    for d in &st.recent {
        out.push_str(&format!("recent={}\n", d.display()));
    }
    out.push_str(&format!("interval={}\nfit={}\n", st.interval, u8::from(st.fit)));
    if let Some(server) = &st.server {
        out.push_str(&format!("server={server}\n"));
    }
    if st.server_own {
        out.push_str("server_folders=own\n");
        for d in &st.server_dirs {
            out.push_str(&format!("server_dir={}\n", d.display()));
        }
        for d in &st.server_exclude {
            out.push_str(&format!("server_exclude={}\n", d.display()));
        }
    }
    if let Some([x, y, w, h]) = st.window {
        out.push_str(&format!("window={x:.0},{y:.0},{w:.0},{h:.0}\n"));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("could not save settings to {}: {e}", path.display());
    }
}
