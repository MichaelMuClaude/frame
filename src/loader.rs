//! Background folder scanning and image decoding (including HEIC/HEIF).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui::{self, ColorImage};
use image::ImageDecoder;

/// Longest side photos are shrunk to before they become textures.
const MAX_SIDE: u32 = 2048;
const WORKERS: usize = 2;
const EXTS: [&str; 10] = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "heic", "heif"];

pub enum Loaded {
    Ok(PathBuf, ColorImage),
    Err(PathBuf, String),
}

/// Scans the folders on a background thread and sends back every photo found,
/// leaving out anything inside an `exclude` folder.
pub fn spawn_scan(ctx: egui::Context, dirs: Vec<PathBuf>, exclude: Vec<PathBuf>) -> Receiver<Vec<PathBuf>> {
    let (scan_tx, scan_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut found = Vec::new();
        for dir in dirs {
            scan(&dir, &exclude, &mut found);
        }
        let _ = scan_tx.send(found);
        ctx.request_repaint();
    });
    scan_rx
}

/// Starts the decoder threads. Returns (job sender, decoded images).
pub fn spawn_workers(ctx: egui::Context) -> (Sender<PathBuf>, Receiver<Loaded>) {
    let (job_tx, job_rx) = mpsc::channel::<PathBuf>();
    let (res_tx, res_rx) = mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for _ in 0..WORKERS {
        let job_rx = Arc::clone(&job_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            loop {
                let job = job_rx.lock().ok().and_then(|rx| rx.recv().ok());
                let Some(path) = job else { break };
                let msg = match decode(&path) {
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

fn scan(dir: &Path, exclude: &[PathBuf], out: &mut Vec<PathBuf>) {
    let walker = walkdir::WalkDir::new(dir)
        .into_iter()
        // Skip hidden folders such as .dropbox.cache and .thumbnails (but not the root itself),
        // and excluded folders together with everything below them.
        .filter_entry(|e| {
            (e.depth() == 0 || !is_hidden(e.file_name())) && !exclude.iter().any(|x| e.path().starts_with(x))
        });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry.path().extension().map(|e| e.to_string_lossy().to_lowercase());
        if ext.is_some_and(|e| EXTS.contains(&e.as_str())) {
            out.push(entry.into_path());
        }
    }
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| e == "heic" || e == "heif")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_excluded_folders_and_everything_below() {
        let root = std::env::temp_dir().join(format!("frame-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for sub in ["keep", "skip", "skip/deep", "keep/skipper"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        for f in ["a.jpg", "keep/b.jpg", "skip/c.jpg", "skip/deep/d.heic", "keep/skipper/e.png"] {
            std::fs::write(root.join(f), b"").unwrap();
        }

        let mut found = Vec::new();
        scan(&root, &[root.join("skip")], &mut found);
        let mut names: Vec<_> = found.iter().map(|p| p.strip_prefix(&root).unwrap().to_owned()).collect();
        names.sort();
        // "keep/skipper" shares a name prefix with "skip" but is a different folder: it must stay.
        assert_eq!(names, [PathBuf::from("a.jpg"), "keep/b.jpg".into(), "keep/skipper/e.png".into()]);

        let mut all = Vec::new();
        scan(&root, &[], &mut all);
        assert_eq!(all.len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }
}
