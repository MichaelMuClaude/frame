//! Background folder scanning and image decoding (including HEIC/HEIF).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui::{self, ColorImage};
use image::ImageDecoder;

use crate::remote::Remote;

/// Longest side photos are shrunk to before they become textures.
const MAX_SIDE: u32 = 2048;
const WORKERS: usize = 2;
const EXTS: [&str; 8] = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff"];
const HEIF_EXTS: [&str; 2] = ["heic", "heif"];

/// HEIC photos are read with libheif when built with the `heic` feature, otherwise on Windows
/// with the system codec.
const HAS_HEIF: bool = cfg!(any(feature = "heic", windows));
/// The error a HEIC photo gives on a Windows without the codec, so the app can explain it once.
pub const HEIF_CODEC_MISSING: &str = "HEIC photos need the HEIF and HEVC extensions from the Microsoft Store";

/// Whether a lower-case file extension is a photo type this build can show.
fn is_photo_ext(ext: &str) -> bool {
    EXTS.contains(&ext) || (HAS_HEIF && HEIF_EXTS.contains(&ext))
}

/// What a scan sends back: the photos found, or why the photo server could not be reached.
pub type Scan = Result<Vec<PathBuf>, String>;

pub enum Loaded {
    Ok(PathBuf, ColorImage),
    Err(PathBuf, String),
}

/// Scans the folders on a background thread and sends back every photo found,
/// leaving out anything inside an `exclude` folder.
pub fn spawn_scan(ctx: egui::Context, dirs: Vec<PathBuf>, exclude: Vec<PathBuf>) -> Receiver<Scan> {
    let (scan_tx, scan_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = scan_tx.send(Ok(collect(&dirs, &exclude)));
        ctx.request_repaint();
    });
    scan_rx
}

/// Asks the photo server for its photo list on a background thread (client mode).
pub fn spawn_remote_scan(ctx: egui::Context, remote: Arc<Remote>, own: Option<(Vec<PathBuf>, Vec<PathBuf>)>) -> Receiver<Scan> {
    let (scan_tx, scan_rx) = mpsc::channel();
    thread::spawn(move || {
        let list = match &own {
            None => remote.list(false),
            Some((dirs, exclude)) => remote.list(true).map(|all| chosen(all, dirs, exclude)),
        };
        let _ = scan_tx.send(list);
        ctx.request_repaint();
    });
    scan_rx
}

/// Client mode, own folders: the server's photos inside `dirs` (all of them when `dirs` is
/// empty) and outside `exclude`.
fn chosen(photos: Vec<PathBuf>, dirs: &[PathBuf], exclude: &[PathBuf]) -> Vec<PathBuf> {
    photos
        .into_iter()
        .filter(|p| dirs.is_empty() || dirs.iter().any(|d| p.starts_with(d)))
        .filter(|p| !exclude.iter().any(|d| p.starts_with(d)))
        .collect()
}

/// Every photo in `dirs`, each listed once even when the folders overlap.
fn collect(dirs: &[PathBuf], exclude: &[PathBuf]) -> Vec<PathBuf> {
    let ex = Exclude::new(exclude);
    let mut found = Vec::new();
    for dir in dirs {
        scan(dir, &ex, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

/// Excluded folders, also in their resolved form, so a folder reached through a symlink
/// (for example a photo folder that is a link into a sync folder) is still recognised.
struct Exclude {
    raw: Vec<PathBuf>,
    real: Vec<PathBuf>,
}

impl Exclude {
    fn new(exclude: &[PathBuf]) -> Self {
        let real = exclude.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
        Self { raw: exclude.to_vec(), real }
    }

    fn contains(&self, entry: &walkdir::DirEntry) -> bool {
        if self.raw.is_empty() {
            return false;
        }
        let path = entry.path();
        if self.raw.iter().any(|x| path.starts_with(x)) {
            return true;
        }
        // Resolving costs a system call, so only folders are checked; their files go with them.
        entry.file_type().is_dir()
            && std::fs::canonicalize(path).is_ok_and(|real| self.real.iter().any(|x| real.starts_with(x)))
    }
}

/// Starts the decoder threads, which read local files or, in client mode, fetch previews from
/// the photo server. Returns (job sender, decoded images).
pub fn spawn_workers(ctx: egui::Context, remote: Option<Arc<Remote>>) -> (Sender<PathBuf>, Receiver<Loaded>) {
    let (job_tx, job_rx) = mpsc::channel::<PathBuf>();
    let (res_tx, res_rx) = mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for _ in 0..WORKERS {
        let job_rx = Arc::clone(&job_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        let remote = remote.clone();
        thread::spawn(move || {
            loop {
                let job = job_rx.lock().ok().and_then(|rx| rx.recv().ok());
                let Some(path) = job else { break };
                let decoded = match &remote {
                    Some(r) => decode_remote(r, &path),
                    None => decode(&path),
                };
                let msg = match decoded {
                    Ok(img) => Loaded::Ok(path, img),
                    Err(e) => Loaded::Err(path, e),
                };
                if res_tx.send(msg).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    }
    (job_tx, res_rx)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn scan(dir: &Path, exclude: &Exclude, out: &mut Vec<PathBuf>) {
    let walker = walkdir::WalkDir::new(dir)
        .into_iter()
        // Skip hidden folders such as .dropbox.cache and .thumbnails (but not the root itself),
        // and excluded folders together with everything below them.
        .filter_entry(|e| (e.depth() == 0 || !is_hidden(e.file_name())) && !exclude.contains(e));
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry.path().extension().map(|e| e.to_string_lossy().to_lowercase());
        if ext.is_some_and(|e| is_photo_ext(&e)) {
            out.push(entry.into_path());
        }
    }
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| HEIF_EXTS.contains(&e.as_str()))
}

fn decode(path: &Path) -> Result<ColorImage, String> {
    let img = if is_heif(path) { decode_heif(path)? } else { decode_std(path)? };
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

fn decode_remote(remote: &Remote, path: &Path) -> Result<ColorImage, String> {
    let bytes = remote.image(path)?;
    // The server's previews are JPEGs already turned the right way up and no bigger than needed.
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

fn decode_std(path: &Path) -> Result<image::DynamicImage, String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img)
}

#[cfg(feature = "heic")]
fn decode_heif(path: &Path) -> Result<image::DynamicImage, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_file(path.to_str().ok_or("non-UTF-8 path")?).map_err(|e| e.to_string())?;
    let handle = ctx.primary_image_handle().map_err(|e| e.to_string())?;
    // libheif applies the rotation/mirroring stored in the file by default.
    let decoded = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)
        .map_err(|e| e.to_string())?;
    let planes = decoded.planes();
    let plane = planes.interleaved.ok_or("no interleaved plane")?;
    let (w, h) = (plane.width as usize, plane.height as usize);
    let mut buf = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = row * plane.stride;
        buf.extend_from_slice(&plane.data[start..start + w * 4]);
    }
    let rgba = image::RgbaImage::from_raw(w as u32, h as u32, buf).ok_or("bad buffer size")?;
    Ok(image::DynamicImage::ImageRgba8(rgba))
}

#[cfg(all(windows, not(feature = "heic")))]
fn decode_heif(path: &Path) -> Result<image::DynamicImage, String> {
    crate::wic::decode(path).map(image::DynamicImage::ImageRgba8)
}

#[cfg(not(any(feature = "heic", windows)))]
fn decode_heif(_path: &Path) -> Result<image::DynamicImage, String> {
    Err("this build has no HEIC support".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_keeps_picked_folders_minus_excluded() {
        let p = |s: &str| PathBuf::from(s);
        let photos = vec![p("/pics/a.jpg"), p("/pics/x/b.jpg"), p("/pics/trip/c.jpg"), p("/pics/trip/skip/d.jpg"), p("/pics/tripod/e.jpg")];
        // No folders picked: the whole library, minus the excluded ones.
        assert_eq!(chosen(photos.clone(), &[], &[p("/pics/x")]), vec![p("/pics/a.jpg"), p("/pics/trip/c.jpg"), p("/pics/trip/skip/d.jpg"), p("/pics/tripod/e.jpg")]);
        // Folder matching is by whole path parts: "trip" does not take in "tripod".
        assert_eq!(chosen(photos, &[p("/pics/trip")], &[p("/pics/trip/skip")]), vec![p("/pics/trip/c.jpg")]);
    }

    #[test]
    fn collect_skips_excluded_folders_and_duplicates() {
        let root = std::env::temp_dir().join(format!("frame-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for sub in ["keep", "skip", "skip/deep", "keep/skipper"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        for f in ["a.jpg", "keep/b.jpg", "skip/c.jpg", "skip/deep/d.webp", "keep/skipper/e.png"] {
            std::fs::write(root.join(f), b"").unwrap();
        }

        let names = |found: Vec<PathBuf>| -> Vec<PathBuf> {
            found.iter().map(|p| p.strip_prefix(&root).unwrap().to_owned()).collect()
        };
        let found = collect(&[root.clone()], &[root.join("skip")]);
        // "keep/skipper" shares a name prefix with "skip" but is a different folder: it must stay.
        assert_eq!(names(found), [PathBuf::from("a.jpg"), "keep/b.jpg".into(), "keep/skipper/e.png".into()]);

        assert_eq!(collect(&[root.clone()], &[]).len(), 5);

        // Overlapping photo folders list each photo once.
        assert_eq!(collect(&[root.clone(), root.join("keep")], &[]).len(), 5);

        // A photo folder reached through a symlink still honours an exclusion given by its real path.
        #[cfg(unix)]
        {
            let link = std::env::temp_dir().join(format!("frame-scan-link-{}", std::process::id()));
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&root, &link).unwrap();
            let via_link = collect(&[link.clone()], &[root.join("skip")]);
            let _ = std::fs::remove_file(&link);
            assert_eq!(via_link.len(), 3, "{via_link:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn heif_photos_come_out_upright() {
        if !HAS_HEIF {
            return;
        }
        let dir = std::env::temp_dir().join(format!("frame-heif-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The second photo is stored sideways with a rotation tag, the way phone cameras save
        // portrait photos. Both have a white arrow at the top centre when shown the right way up.
        let photos: [(&str, &[u8], [usize; 2]); 2] = [
            ("landscape.heic", include_bytes!("../tests/data/landscape.heic"), [1600, 1000]),
            ("portrait-rotated.heic", include_bytes!("../tests/data/portrait-rotated.heic"), [1000, 1600]),
        ];
        for (name, bytes, size) in photos {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let img = match decode(&path) {
                Err(e) if e == HEIF_CODEC_MISSING => {
                    eprintln!("skipped: {e}");
                    break;
                }
                other => other.unwrap(),
            };
            assert_eq!(img.size, size, "{name}");
            let arrow = img.pixels[150 * size[0] + size[0] / 2];
            assert!(arrow.r() > 200 && arrow.g() > 200 && arrow.b() > 200, "{name} is not upright: {arrow:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
