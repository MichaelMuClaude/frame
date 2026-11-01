//! A small, resizable desktop photo frame: shows random photos from a folder
//! and lets you send the current one to the Trash by hovering and clicking.

// Windows: a GUI program, so double-clicking the .exe does not open a console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod fonts;
mod loader;
mod reveal;
mod settings;
#[cfg(windows)]
mod wic;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, ResizeDirection, Sense, TextureHandle,
    TextureOptions, ViewportCommand, vec2,
};
use rand::seq::SliceRandom;

use loader::Loaded;
use settings::{Settings, home_dir};

const MAT: Color32 = Color32::from_rgb(238, 232, 220);
const GOLD: Color32 = Color32::from_rgb(196, 158, 78);
const WOOD_TOP: Color32 = Color32::from_rgb(150, 101, 56);
const WOOD_LEFT: Color32 = Color32::from_rgb(130, 85, 46);
const WOOD_RIGHT: Color32 = Color32::from_rgb(98, 62, 33);
const WOOD_BOTTOM: Color32 = Color32::from_rgb(82, 51, 27);
const EDGE: f32 = 7.0;
const TOAST_SECS: u64 = 8;
/// How long the window has to stay put after a move or resize before its geometry is saved.
const GEOMETRY_SAVE_DELAY: Duration = Duration::from_millis(1500);

/// What the Undo button in the toast can reverse.
enum Undo {
    /// The photo, and the Live Photo video that went with it.
    Trashed(PathBuf, Option<PathBuf>),
    /// `video` holds the Live Photo video's old and new paths.
    Moved { from: PathBuf, to: PathBuf, video: Option<(PathBuf, PathBuf)> },
}

/// What a folder-picker dialog was opened for.
enum Pick {
    MoveTo(PathBuf),
    AddSource,
    Exclude,
}

struct Config {
    dirs: Vec<PathBuf>,
    interval: Duration,
    /// Keep the window at normal stacking level instead of sinking it below other windows.
    normal_level: bool,
    settings: Settings,
}

struct App {
    queue: Vec<PathBuf>,
    pos: usize,
    scan_rx: Receiver<Vec<PathBuf>>,
    scanned: bool,
    job_tx: Sender<PathBuf>,
    res_rx: Receiver<Loaded>,
    cache: HashMap<PathBuf, TextureHandle>,
    pending: HashSet<PathBuf>,
    shown: Option<(PathBuf, TextureHandle)>,
    shown_at: Instant,
    interval: Duration,
    paused: bool,
    toast: Option<(String, Instant, bool)>,
    last_action: Option<Undo>,
    dirs: Vec<PathBuf>,
    settings: Settings,
    settings_open: bool,
    move_menu: bool,
    /// Whether the pointer was over the photo last frame; the slideshow waits while it is.
    hover_photo: bool,
    pick: Option<(Pick, Receiver<Option<PathBuf>>)>,
    frames: u32,
    sink: bool,
    /// Resize the frame to match each photo's orientation.
    fit: bool,
    /// Aspect ratio of a newly shown photo that the window has not been fitted to yet.
    fit_aspect: Option<f32>,
    /// Photos shown before the current one, most recent last.
    history: Vec<PathBuf>,
    /// Result of a "Show in folder" running in the background.
    reveal_rx: Option<Receiver<Result<(), String>>>,
    /// Window position and size last written to the settings, and a change waiting to be saved.
    saved_geometry: Option<[f32; 4]>,
    geometry_changed: Option<([f32; 4], Instant)>,
    /// Whether the user has been told that Windows lacks the HEIC codec.
    heif_warned: bool,
}

impl App {
    fn new(ctx: &egui::Context, cfg: Config) -> Self {
        let scan_rx = loader::spawn_scan(ctx.clone(), cfg.dirs.clone(), cfg.settings.exclude.clone());
        let (job_tx, res_rx) = loader::spawn_workers(ctx.clone());
        Self {
            queue: Vec::new(),
            pos: 0,
            scan_rx,
            scanned: false,
            job_tx,
            res_rx,
            cache: HashMap::new(),
            pending: HashSet::new(),
            shown: None,
            shown_at: Instant::now(),
            interval: cfg.interval,
            paused: false,
            toast: None,
            last_action: None,
            dirs: cfg.dirs,
            move_menu: false,
            hover_photo: false,
            settings_open: false,
            pick: None,
            frames: 0,
            sink: !cfg.normal_level,
            fit: cfg.settings.fit,
            fit_aspect: None,
            history: Vec::new(),
            reveal_rx: None,
            saved_geometry: cfg.settings.window,
            geometry_changed: None,
            heif_warned: false,
            settings: cfg.settings,
        }
    }

    /// Reshapes the window to the photo's aspect ratio, keeping its centre and photo area.
    fn fit_window(&self, ctx: &egui::Context, aspect: f32) {
        let Some(inner) = ctx.input(|i| i.viewport().inner_rect) else { return };
        let cur = inner.size();
        let pad = frame_pad(cur);
        let area = (cur.x - pad).max(60.0) * (cur.y - pad).max(60.0);
        let pw = (area * aspect).sqrt();
        let ph = pw / aspect;
        let mut size = vec2(pw + pad, ph + pad);
        for _ in 0..3 {
            let q = frame_pad(size);
            size = vec2(pw + q, ph + q);
        }
        let limit = ctx
            .input(|i| i.viewport().monitor_size)
            .map_or(vec2(2400.0, 1600.0), |m| m * 0.9);
        size = size.max(vec2(200.0, 160.0)).min(limit);
        if (size - cur).abs().max_elem() < 2.0 {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
            let mut pos = outer.center() - size / 2.0;
            // Growing taller around the centre must not push the top (and its buttons) off screen.
            pos.y = pos.y.max(0.0);
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
    }

    fn current_path(&self) -> Option<&PathBuf> {
        self.queue.get(self.pos)
    }

    fn request(&mut self, path: PathBuf) {
        if !self.cache.contains_key(&path) && self.pending.insert(path.clone()) {
            let _ = self.job_tx.send(path);
        }
    }

    fn prefetch(&mut self) {
        let n = self.queue.len();
        if n == 0 {
            return;
        }
        for off in 0..=1 {
            let p = self.queue[(self.pos + off) % n].clone();
            self.request(p);
        }
        // Keep only the photos near the cursor in memory.
        let keep: HashSet<&PathBuf> = [n - 1, 0, 1, 2]
            .iter()
            .map(|o| &self.queue[(self.pos + o) % n])
            .collect();
        self.cache.retain(|k, _| keep.contains(k));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let picked = self.pick.as_ref().and_then(|(_, rx)| match rx.try_recv() {
            Ok(res) => Some(res),
            Err(mpsc::TryRecvError::Disconnected) => Some(None),
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(res) = picked
            && let Some((kind, _)) = self.pick.take()
                && let Some(dir) = res {
                    match kind {
                        Pick::MoveTo(photo) => self.move_current_to(photo, dir),
                        Pick::AddSource => {
                            if !self.dirs.contains(&dir) {
                                self.dirs.push(dir);
                                self.save_dirs(ctx);
                            }
                        }
                        Pick::Exclude => {
                            if !self.settings.exclude.contains(&dir) {
                                self.settings.exclude.push(dir);
                                self.save_exclude(ctx);
                            }
                        }
                    }
                }
        if let Some(Ok(res)) = self.reveal_rx.as_ref().map(|rx| rx.try_recv()) {
            self.reveal_rx = None;
            if let Err(e) = res {
                self.toast = Some((format!("Could not open the folder: {e}"), Instant::now(), false));
            }
        }
        while let Ok(mut batch) = self.scan_rx.try_recv() {
            batch.shuffle(&mut rand::rng());
            self.queue = batch;
            self.pos = 0;
            self.scanned = true;
        }
        while let Ok(res) = self.res_rx.try_recv() {
            match res {
                Loaded::Ok(path, img) => {
                    self.pending.remove(&path);
                    let tex = ctx.load_texture(path.to_string_lossy(), img, TextureOptions::LINEAR);
                    self.cache.insert(path, tex);
                }
                Loaded::Err(path, err) => {
                    eprintln!("skip {}: {err}", path.display());
                    if err == loader::HEIF_CODEC_MISSING && !self.heif_warned {
                        self.heif_warned = true;
                        self.toast = Some((err, Instant::now(), false));
                    }
                    self.pending.remove(&path);
                    self.remove_from_queue(&path);
                }
            }
        }
        self.prefetch();
        if let Some(p) = self.current_path().cloned() {
            let already = self.shown.as_ref().is_some_and(|(sp, _)| *sp == p);
            if !already
                && let Some(tex) = self.cache.get(&p) {
                    self.fit_aspect = Some(tex.aspect_ratio());
                    self.shown = Some((p, tex.clone()));
                    self.shown_at = Instant::now();
                }
        }
    }

    fn remove_from_queue(&mut self, path: &PathBuf) {
        if let Some(i) = self.queue.iter().position(|p| p == path) {
            self.queue.remove(i);
            if i < self.pos {
                self.pos -= 1;
            }
            if self.pos >= self.queue.len() {
                self.pos = 0;
            }
        }
    }

    fn step(&mut self, forward: bool) {
        let n = self.queue.len();
        if n == 0 {
            return;
        }
        if forward {
            if let Some((p, _)) = &self.shown
                && self.history.last() != Some(p) {
                    self.history.push(p.clone());
                    if self.history.len() > 100 {
                        self.history.remove(0);
                    }
                }
            self.pos += 1;
            if self.pos >= n {
                self.pos = 0;
                self.queue.shuffle(&mut rand::rng());
                // A new round should not start with the photo that is on screen now.
                if n > 1 && self.shown.as_ref().is_some_and(|(p, _)| *p == self.queue[0]) {
                    self.queue.swap(0, n - 1);
                }
            }
        } else {
            self.pos = (self.pos + n - 1) % n;
        }
        // Prevent the auto-advance timer from firing again before the new photo shows.
        self.shown_at = Instant::now();
    }

    /// Goes back to the photo that was on screen just before this one.
    fn go_back(&mut self) {
        while let Some(p) = self.history.pop() {
            if let Some(i) = self.queue.iter().position(|q| *q == p) {
                self.pos = i;
                self.shown_at = Instant::now();
                return;
            }
        }
        self.step(false);
    }

    fn delete_current(&mut self) {
        let Some((path, _)) = self.shown.clone() else { return };
        let video = live_video(&path);
        let result = match &video {
            Some(v) => trash::delete_all([&path, v]),
            None => trash::delete(&path),
        };
        match result {
            Ok(()) => {
                self.shown = None;
                self.cache.remove(&path);
                self.remove_from_queue(&path);
                let msg = if video.is_some() { "Moved to Trash (with its video)" } else { "Moved to Trash" };
                self.last_action = Some(Undo::Trashed(path, video));
                self.toast = Some((msg.into(), Instant::now(), true));
            }
            Err(e) => {
                self.toast = Some((format!("Trash failed: {e}"), Instant::now(), false));
            }
        }
    }

    fn busy(&self) -> bool {
        self.move_menu || self.pick.is_some()
    }

    fn undo(&mut self) {
        let msg = match self.last_action.take() {
            Some(Undo::Trashed(path, video)) => self.undo_trash(path, video),
            Some(Undo::Moved { from, to, video }) => {
                let dest = free_name(&from, video.as_ref().map(|(old, _)| old.as_path()));
                match move_file(&to, &dest) {
                    Ok(()) => {
                        let note = match &video {
                            Some((_, now)) => match move_file(now, &with_video_ext(&dest, now)) {
                                Ok(()) => "",
                                Err(_) => " (its video stayed behind)",
                            },
                            None => "",
                        };
                        self.reinsert(dest);
                        format!("Moved back{note}")
                    }
                    Err(e) => format!("Undo failed: {e}"),
                }
            }
            None => return,
        };
        self.toast = Some((msg, Instant::now(), false));
    }

    /// Puts a restored photo right after the current one and shows it.
    fn reinsert(&mut self, path: PathBuf) {
        let at = (self.pos + 1).min(self.queue.len());
        self.queue.insert(at, path);
        self.step(true);
    }

    #[cfg(not(target_os = "macos"))]
    fn undo_trash(&mut self, target: PathBuf, video: Option<PathBuf>) -> String {
        let mut items = match trash::os_limited::list() {
            Ok(items) => items,
            Err(e) => return format!("Restore failed: {e}"),
        };
        items.sort_by_key(|i| i.time_deleted);
        let mut restore = Vec::new();
        for want in std::iter::once(&target).chain(video.as_ref()) {
            if let Some(i) = items.iter().rposition(|it| it.original_path() == *want) {
                restore.push(items.remove(i));
            }
        }
        if !restore.iter().any(|it| it.original_path() == target) {
            return "Not found in Trash".to_string();
        }
        match trash::os_limited::restore_all(restore) {
            Ok(()) => {
                self.reinsert(target);
                "Restored".to_string()
            }
            Err(e) => format!("Restore failed: {e}"),
        }
    }

    #[cfg(target_os = "macos")]
    fn undo_trash(&mut self, _target: PathBuf, _video: Option<PathBuf>) -> String {
        "Undo not supported here".to_string()
    }

    fn move_current_to(&mut self, photo: PathBuf, dir: PathBuf) {
        let Some(name) = photo.file_name() else { return };
        let target = dir.join(name);
        if target == photo {
            self.toast = Some(("Already in that folder".into(), Instant::now(), false));
            return;
        }
        let video = live_video(&photo);
        let dest = free_name(&target, video.as_deref());
        match move_file(&photo, &dest) {
            Ok(()) => {
                // A Live Photo's video keeps the photo's name so the pair stays together.
                let mut video_moved = None;
                let mut video_err = None;
                if let Some(v) = &video {
                    let vdest = with_video_ext(&dest, v);
                    match move_file(v, &vdest) {
                        Ok(()) => video_moved = Some((v.clone(), vdest)),
                        Err(e) => video_err = Some(e),
                    }
                }
                self.cache.remove(&photo);
                if self.shown.as_ref().is_some_and(|(p, _)| *p == photo) {
                    self.shown = None;
                }
                self.remove_from_queue(&photo);
                let label = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.display().to_string());
                let msg = match (&video_moved, &video_err) {
                    (_, Some(e)) => format!("Moved to {label}, but its video failed: {e}"),
                    (Some(_), None) => format!("Moved to {label} (with its video)"),
                    (None, None) => format!("Moved to {label}"),
                };
                self.last_action = Some(Undo::Moved { from: photo, to: dest, video: video_moved });
                self.toast = Some((msg, Instant::now(), true));
                self.settings.recent.retain(|p| *p != dir);
                self.settings.recent.insert(0, dir);
                self.settings.recent.truncate(8);
                settings::save(&self.settings);
            }
            Err(e) => self.toast = Some((format!("Move failed: {e}"), Instant::now(), false)),
        }
    }

    /// Opens the system folder picker without blocking the UI.
    fn start_pick(&mut self, ctx: &egui::Context, kind: Pick) {
        if self.pick.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start = match kind {
            // Folders to leave out sit inside the photo folders, so start there.
            Pick::Exclude => self.dirs.first().cloned(),
            _ => self.settings.recent.first().cloned(),
        }
        .or_else(home_dir);
        let title = match kind {
            Pick::MoveTo(_) => "Move photo to...",
            Pick::AddSource => "Add a photo folder",
            Pick::Exclude => "Exclude a folder",
        };
        self.pick = Some((kind, rx));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        });
    }

    /// Opens the file manager with the current photo selected. Runs in the background because
    /// the file manager may first have to start, which can take a few seconds.
    fn show_current_in_folder(&mut self, ctx: &egui::Context) {
        let Some((path, _)) = self.shown.clone() else { return };
        let (tx, rx) = mpsc::channel();
        self.reveal_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(reveal::show_in_folder(&path));
            ctx.request_repaint();
        });
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        self.scan_rx = loader::spawn_scan(ctx.clone(), self.dirs.clone(), self.settings.exclude.clone());
        self.scanned = false;
    }

    fn save_exclude(&mut self, ctx: &egui::Context) {
        settings::save(&self.settings);
        self.rescan(ctx);
    }

    fn save_dirs(&mut self, ctx: &egui::Context) {
        self.settings.dirs = self.dirs.clone();
        settings::save(&self.settings);
        self.rescan(ctx);
    }

    fn draw_move_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, painter: &egui::Painter, mv: Rect, bounds: Rect) {
        let Some((photo, _)) = self.shown.clone() else {
            self.move_menu = false;
            return;
        };
        let recents: Vec<PathBuf> = self.settings.recent.iter().filter(|p| p.is_dir()).take(4).cloned().collect();
        let (row_h, head_h) = (28.0, 26.0);
        let w = 230.0_f32.min(bounds.width() - 20.0);
        let h = head_h + row_h * (recents.len() as f32 + 1.0) + 8.0;
        let mut top = mv.bottom() + 4.0;
        if top + h > bounds.bottom() - 4.0 {
            top = (bounds.bottom() - 4.0 - h).max(bounds.top() + 4.0);
        }
        let menu = Rect::from_min_size(Pos2::new(mv.left(), top), vec2(w, h));
        painter.rect_filled(menu, 8.0, Color32::from_black_alpha(230));
        painter.text(
            menu.min + vec2(12.0, head_h / 2.0 + 2.0),
            Align2::LEFT_CENTER,
            "Move to:",
            FontId::proportional(12.0),
            Color32::from_gray(170),
        );
        let mut chosen: Option<PathBuf> = None;
        let mut browse = false;
        let mut y = menu.top() + head_h + 2.0;
        let row = |ui: &mut egui::Ui, id: Id, label: String, y: f32| -> bool {
            let r = Rect::from_min_size(Pos2::new(menu.left() + 4.0, y), vec2(w - 8.0, row_h));
            let resp = ui.interact(r, id, Sense::click());
            if resp.hovered() {
                painter.rect_filled(r, 5.0, Color32::from_white_alpha(40));
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            painter.with_clip_rect(r.shrink2(vec2(4.0, 0.0))).text(
                r.left_center() + vec2(8.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(14.0),
                Color32::WHITE,
            );
            resp.clicked()
        };
        for (i, dir) in recents.iter().enumerate() {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            if row(ui, Id::new(("move-row", i)), name, y) {
                chosen = Some(dir.clone());
            }
            y += row_h;
        }
        if row(ui, Id::new("move-browse"), "Choose folder...".into(), y) {
            browse = true;
        }
        let clicked_outside = ctx.input(|i| {
            i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| !menu.contains(p) && !mv.contains(p))
        });
        if let Some(dir) = chosen {
            self.move_menu = false;
            self.move_current_to(photo, dir);
        } else if browse {
            self.move_menu = false;
            self.start_pick(ctx, Pick::MoveTo(photo));
        } else if clicked_outside {
            self.move_menu = false;
        }
    }

    /// Saves the window position and size once it has stopped moving, so the next start
    /// opens the frame where it was left (also after a crash).
    fn track_geometry(&mut self, ctx: &egui::Context) {
        if self.frames < 10 {
            return;
        }
        let Some((outer, inner)) = ctx.input(|i| i.viewport().outer_rect.zip(i.viewport().inner_rect)) else {
            return;
        };
        let now = [outer.min.x, outer.min.y, inner.width(), inner.height()];
        let same = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.0);
        if self.saved_geometry.is_some_and(|s| same(s, now)) {
            self.geometry_changed = None;
            return;
        }
        match self.geometry_changed {
            Some((g, since)) if same(g, now) => {
                if since.elapsed() >= GEOMETRY_SAVE_DELAY {
                    self.saved_geometry = Some(now);
                    self.settings.window = Some(now);
                    self.geometry_changed = None;
                    settings::save(&self.settings);
                }
            }
            _ => self.geometry_changed = Some((now, Instant::now())),
        }
    }

    fn settings_viewport(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Frame settings")
            .with_inner_size([540.0, 500.0])
            .with_min_inner_size([380.0, 320.0]);
        let dirs = self.dirs.clone();
        let exclude = self.settings.exclude.clone();
        let mut secs = self.interval.as_secs();
        let mut fit = self.fit;
        let (mut close, mut add, mut remove) = (false, false, None::<usize>);
        let (mut add_exclude, mut unexclude) = (false, None::<usize>);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("frame-settings"), builder, |ctx, _| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Photo folders");
                ui.label("Photos are picked at random from these folders and everything inside them.");
                ui.add_space(6.0);
                for (i, d) in dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirs.len() > 1, egui::Button::new("Remove")).clicked() {
                            remove = Some(i);
                        }
                        ui.label(d.display().to_string());
                    });
                }
                ui.add_space(4.0);
                if ui.button("Add folder...").clicked() {
                    add = true;
                }
                ui.separator();
                ui.heading("Excluded folders");
                ui.label("Photos in these folders, and in the folders below them, are never shown.");
                ui.add_space(6.0);
                if exclude.is_empty() {
                    ui.weak("None");
                }
                for (i, d) in exclude.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button("Include again").clicked() {
                            unexclude = Some(i);
                        }
                        ui.label(d.display().to_string());
                    });
                }
                ui.add_space(4.0);
                if ui.button("Exclude folder...").clicked() {
                    add_exclude = true;
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Change photo every");
                    ui.add(egui::DragValue::new(&mut secs).range(1..=3600).suffix(" s"));
                });
                ui.checkbox(&mut fit, "Fit the frame to each photo's shape");
                ui.separator();
                if let Some(p) = settings::file_path() {
                    ui.weak(format!("Saved in {}", p.display()));
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if ctx.input(|i| i.viewport().close_requested()) {
                close = true;
            }
        });
        if let Some(i) = remove
            && i < self.dirs.len() && self.dirs.len() > 1 {
                self.dirs.remove(i);
                self.save_dirs(ctx);
            }
        if add {
            self.start_pick(ctx, Pick::AddSource);
        }
        if let Some(i) = unexclude
            && i < self.settings.exclude.len() {
                self.settings.exclude.remove(i);
                self.save_exclude(ctx);
            }
        if add_exclude {
            self.start_pick(ctx, Pick::Exclude);
        }
        if secs != self.interval.as_secs() {
            self.interval = Duration::from_secs(secs.max(1));
            self.settings.interval = secs.max(1);
            settings::save(&self.settings);
        }
        if fit != self.fit {
            self.fit = fit;
            self.settings.fit = fit;
            settings::save(&self.settings);
        }
        if close {
            self.settings_open = false;
        }
    }
}

/// Width of the wooden frame band, scaled with the window.
fn frame_width(rect: Rect) -> f32 {
    (rect.width().min(rect.height()) * 0.055).clamp(10.0, 34.0)
}

/// Paints a mitred wooden picture frame with a gold lip and a cream mat.
/// Returns the area left for the photo.
fn paint_frame(p: &egui::Painter, rect: Rect) -> Rect {
    let w = frame_width(rect);
    let o = rect;
    let i = rect.shrink(w);
    let quad = |a: Pos2, b: Pos2, c: Pos2, d: Pos2, col: Color32| {
        p.add(egui::Shape::convex_polygon(vec![a, b, c, d], col, egui::Stroke::NONE));
    };
    quad(o.left_top(), o.right_top(), i.right_top(), i.left_top(), WOOD_TOP);
    quad(o.right_top(), o.right_bottom(), i.right_bottom(), i.right_top(), WOOD_RIGHT);
    quad(o.right_bottom(), o.left_bottom(), i.left_bottom(), i.right_bottom(), WOOD_BOTTOM);
    quad(o.left_bottom(), o.left_top(), i.left_top(), i.left_bottom(), WOOD_LEFT);
    p.rect_stroke(o, 0.0, egui::Stroke::new(1.0, Color32::from_rgb(45, 28, 14)), egui::StrokeKind::Inside);
    // Mat inside the frame, with a gold lip where wood meets mat.
    p.rect_filled(i, 0.0, MAT);
    p.rect_stroke(i, 0.0, egui::Stroke::new(2.0, GOLD), egui::StrokeKind::Inside);
    p.rect_stroke(
        i.shrink(2.0),
        0.0,
        egui::Stroke::new(1.0, Color32::from_black_alpha(60)),
        egui::StrokeKind::Inside,
    );
    i.shrink((w * 0.7).max(6.0))
}

/// Space taken by the frame and mat on both sides of the photo, for a window of this size.
fn frame_pad(size: egui::Vec2) -> f32 {
    let w = frame_width(Rect::from_min_size(Pos2::ZERO, size));
    2.0 * (w + (w * 0.7).max(6.0))
}

fn resize_zone(rect: Rect, p: Pos2) -> Option<ResizeDirection> {
    let left = p.x - rect.min.x < EDGE;
    let right = rect.max.x - p.x < EDGE;
    let top = p.y - rect.min.y < EDGE;
    let bottom = rect.max.y - p.y < EDGE;
    use ResizeDirection::*;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(NorthWest),
        (_, true, true, _) => Some(NorthEast),
        (true, _, _, true) => Some(SouthWest),
        (_, true, _, true) => Some(SouthEast),
        (true, ..) => Some(West),
        (_, true, ..) => Some(East),
        (_, _, true, _) => Some(North),
        (_, _, _, true) => Some(South),
        _ => None,
    }
}

fn resize_cursor(d: ResizeDirection) -> CursorIcon {
    match d {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Some X11 window managers drop the level requested at creation, so ask again once mapped.
        self.frames += 1;
        if self.sink && self.frames == 3 {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnBottom));
        }
        if self.sink {
            keep_at_bottom(&ctx, frame);
        }
        self.poll(&ctx);
        if let Some(a) = self.fit_aspect.take()
            && self.fit {
                // Extremely wide or tall photos keep their letterbox instead of a sliver of a frame.
                self.fit_window(&ctx, a.clamp(0.5, 2.0));
            }

        // Keyboard shortcuts.
        let (next, prev, del, pause) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Space),
            )
        });
        if next {
            self.step(true);
        }
        if prev {
            self.go_back();
        }
        if del {
            self.delete_current();
        }
        if pause {
            self.paused = !self.paused;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.move_menu = false;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
            self.undo();
        }

        // Auto-advance once the current photo has been on screen long enough.
        let showing_current = self.current_path().zip(self.shown.as_ref()).is_some_and(|(c, (s, _))| c == s);
        if self.hover_photo {
            // Hold the timer so the photo gets a full interval once the pointer leaves.
            self.shown_at = Instant::now();
        }
        if !self.paused && !self.busy() && showing_current && self.shown_at.elapsed() >= self.interval && self.queue.len() > 1 {
            self.step(true);
        }

        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        let photo_area = paint_frame(&painter, rect);
        let mut shown_rect: Option<Rect> = None;

        match &self.shown {
            Some((_, tex)) => {
                let size = tex.size_vec2();
                let scale = (photo_area.width() / size.x).min(photo_area.height() / size.y);
                let img_rect = Rect::from_center_size(photo_area.center(), size * scale);
                shown_rect = Some(img_rect);
                painter.image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                painter.rect_stroke(
                    img_rect,
                    0.0,
                    egui::Stroke::new(1.0, Color32::from_black_alpha(110)),
                    egui::StrokeKind::Outside,
                );
            }
            None => {
                let msg = if !self.scanned {
                    "Scanning photos..."
                } else if self.queue.is_empty() && self.heif_warned {
                    "No photos found\n\nHEIC photos need the HEIF and HEVC\nextensions from the Microsoft Store"
                } else if self.queue.is_empty() {
                    "No photos found"
                } else {
                    "Loading..."
                };
                painter.text(
                    photo_area.center(),
                    Align2::CENTER_CENTER,
                    msg,
                    FontId::proportional(16.0),
                    Color32::from_rgb(110, 100, 85),
                );
            }
        }

        // Background: drag to move, right-click for menu.
        let bg = ui.interact(rect, Id::new("bg"), Sense::click_and_drag());
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let zone = pointer.and_then(|p| resize_zone(rect, p));
        if let Some(dir) = zone {
            ctx.set_cursor_icon(resize_cursor(dir));
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        } else if bg.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if bg.double_clicked() {
            self.step(true);
        }
        bg.context_menu(|ui| {
            if ui.button("Next photo").clicked() {
                self.step(true);
                ui.close();
            }
            if ui.button("Previous photo").clicked() {
                self.go_back();
                ui.close();
            }
            if ui.checkbox(&mut self.fit, "Fit frame to photo").changed() && self.fit {
                self.fit_aspect = self.shown.as_ref().map(|(_, t)| t.aspect_ratio());
            }
            let label = if self.paused { "Resume slideshow" } else { "Pause slideshow" };
            if ui.button(label).clicked() {
                self.paused = !self.paused;
                ui.close();
            }
            if ui.add_enabled(self.shown.is_some(), egui::Button::new("Show in folder")).clicked() {
                self.show_current_in_folder(&ctx);
                ui.close();
            }
            ui.separator();
            if ui.button("Settings...").clicked() {
                self.settings_open = true;
                ui.close();
            }
            if ui.button("Quit").clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        });

        // Hover overlay (only while the pointer is over the photo itself): file name and the Trash button.
        let overlay = shown_rect.unwrap_or(photo_area);
        let hovering = shown_rect.zip(pointer).is_some_and(|(r, p)| r.contains(p));
        self.hover_photo = hovering;
        if (hovering || self.move_menu) && self.shown.is_some() {
            if let Some((path, _)) = &self.shown {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                // One line, cut with "…" so a long name stops before the Show in folder button.
                let mut job = egui::text::LayoutJob::simple_singleline(name, FontId::proportional(12.0), Color32::WHITE);
                job.wrap = egui::text::TextWrapping {
                    max_width: (overlay.width() - 150.0).max(40.0),
                    max_rows: 1,
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                let galley = painter.layout_job(job);
                let r = Rect::from_min_size(
                    overlay.left_bottom() + vec2(6.0, -galley.size().y - 8.0),
                    galley.size() + vec2(10.0, 6.0),
                );
                painter.rect_filled(r, 4.0, Color32::from_black_alpha(150));
                painter.galley(r.min + vec2(5.0, 3.0), galley, Color32::WHITE);
            }
            if self.paused {
                let r = Rect::from_center_size(overlay.center_top() + vec2(0.0, 22.0), vec2(70.0, 24.0));
                painter.rect_filled(r, 6.0, Color32::from_black_alpha(150));
                painter.text(r.center(), Align2::CENTER_CENTER, "Paused", FontId::proportional(13.0), Color32::WHITE);
            }
            // "Previous photo" button: a round chevron at the left edge of the photo.
            let c = overlay.left_center() + vec2(26.0, 0.0);
            let prev_resp = ui.interact(Rect::from_center_size(c, vec2(38.0, 38.0)), Id::new("prev"), Sense::click());
            let prev_fill = if prev_resp.hovered() { Color32::from_black_alpha(190) } else { Color32::from_black_alpha(120) };
            painter.circle_filled(c, 18.0, prev_fill);
            let stroke = egui::Stroke::new(2.5, Color32::WHITE);
            painter.line_segment([c + vec2(3.5, -8.0), c + vec2(-3.5, 0.0)], stroke);
            painter.line_segment([c + vec2(-3.5, 0.0), c + vec2(3.5, 8.0)], stroke);
            if prev_resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if prev_resp.clicked() {
                self.go_back();
            }
            let btn = Rect::from_min_size(overlay.right_top() + vec2(-76.0, 8.0), vec2(68.0, 28.0));
            let resp = ui.interact(btn, Id::new("trash"), Sense::click());
            let fill = if resp.hovered() {
                Color32::from_rgb(210, 50, 50)
            } else {
                Color32::from_rgba_unmultiplied(160, 40, 40, 215)
            };
            painter.rect_filled(btn, 6.0, fill);
            painter.text(btn.center(), Align2::CENTER_CENTER, "Trash", FontId::proportional(14.0), Color32::WHITE);
            if resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.delete_current();
            }
            // "Move" button (top-left): pick a folder to move this photo to.
            let mv = Rect::from_min_size(overlay.left_top() + vec2(8.0, 8.0), vec2(64.0, 28.0));
            let mv_resp = ui.interact(mv, Id::new("move"), Sense::click());
            let mv_fill = if mv_resp.hovered() || self.move_menu {
                Color32::from_rgb(70, 110, 170)
            } else {
                Color32::from_rgba_unmultiplied(55, 85, 135, 215)
            };
            painter.rect_filled(mv, 6.0, mv_fill);
            painter.text(mv.center(), Align2::CENTER_CENTER, "Move", FontId::proportional(14.0), Color32::WHITE);
            if mv_resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if mv_resp.clicked() {
                self.move_menu = !self.move_menu;
            }
            if self.move_menu {
                self.draw_move_menu(ui, &ctx, &painter, mv, rect);
            }
            // "Show in folder" button (bottom-right): reveal the photo in the file manager.
            let folder = Rect::from_min_size(overlay.right_bottom() + vec2(-122.0, -36.0), vec2(114.0, 28.0));
            let folder_resp = ui.interact(folder, Id::new("show-in-folder"), Sense::click());
            let folder_fill = if folder_resp.hovered() {
                Color32::from_rgba_unmultiplied(90, 90, 90, 235)
            } else {
                Color32::from_black_alpha(150)
            };
            painter.rect_filled(folder, 6.0, folder_fill);
            painter.text(folder.center(), Align2::CENTER_CENTER, "Show in folder", FontId::proportional(13.0), Color32::WHITE);
            if folder_resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if folder_resp.clicked() {
                self.show_current_in_folder(&ctx);
            }
        }

        // Toast with Undo.
        if let Some((msg, at, undoable)) = self.toast.clone() {
            if at.elapsed() > Duration::from_secs(TOAST_SECS) {
                self.toast = None;
            } else {
                let text_w = painter.layout_no_wrap(msg.clone(), FontId::proportional(13.0), Color32::WHITE).size().x;
                let w = (text_w + 28.0 + if undoable { 62.0 } else { 0.0 }).min(photo_area.width());
                // Above the bottom row (file name, Show in folder) so it never covers those.
                let r = Rect::from_center_size(photo_area.center_bottom() + vec2(0.0, -62.0), vec2(w, 32.0));
                painter.rect_filled(r, 8.0, Color32::from_black_alpha(215));
                painter.text(
                    r.left_center() + vec2(12.0, 0.0),
                    Align2::LEFT_CENTER,
                    msg,
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
                if undoable {
                    let ub = Rect::from_min_size(r.right_top() + vec2(-58.0, 4.0), vec2(50.0, 24.0));
                    let resp = ui.interact(ub, Id::new("undo"), Sense::click());
                    let fill = if resp.hovered() { Color32::from_gray(90) } else { Color32::from_gray(60) };
                    painter.rect_filled(ub, 5.0, fill);
                    painter.text(ub.center(), Align2::CENTER_CENTER, "Undo", FontId::proportional(13.0), Color32::WHITE);
                    if resp.clicked() {
                        self.undo();
                    }
                }
            }
        }

        self.settings_viewport(&ctx);
        self.track_geometry(&ctx);

        // Draw again only when something is due (input, loading and dialogs wake the window
        // themselves), instead of redrawing twice a second forever.
        let mut next = Duration::from_secs(60);
        if self.frames < 10 {
            next = Duration::from_millis(100);
        }
        // The slideshow moved on during this frame (timer, key, Trash...): show the new photo
        // right away instead of at the next timer tick.
        let showing_now = self.current_path().zip(self.shown.as_ref()).is_some_and(|(c, (s, _))| c == s);
        if !showing_now && !self.queue.is_empty() {
            next = next.min(Duration::from_millis(30));
        }
        if let Some((_, at, _)) = &self.toast {
            next = next.min(Duration::from_secs(TOAST_SECS).saturating_sub(at.elapsed()) + Duration::from_millis(50));
        }
        if !self.paused && !self.busy() && !self.hover_photo && showing_current && self.queue.len() > 1 {
            next = next.min(self.interval.saturating_sub(self.shown_at.elapsed()) + Duration::from_millis(20));
        }
        if let Some((_, since)) = self.geometry_changed {
            next = next.min(GEOMETRY_SAVE_DELAY.saturating_sub(since.elapsed()) + Duration::from_millis(50));
        }
        ctx.request_repaint_after(next);
    }
}

/// Windows raises a window whenever it is clicked or activated, and winit only sets "stay at the
/// bottom" once, so after each click or activation the frame is pushed back under other windows.
#[cfg(windows)]
fn keep_at_bottom(ctx: &egui::Context, frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos};

    let (released, focus_changed) = ctx.input(|i| {
        let focused = i.viewport().focused;
        let changed = i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(_)));
        (i.pointer.any_released(), changed || focused.is_none())
    });
    if !released && !focus_changed {
        return;
    }
    let Ok(handle) = frame.window_handle() else { return };
    if let RawWindowHandle::Win32(h) = handle.as_raw() {
        // SAFETY: the handle belongs to this live window; SetWindowPos only changes its z-order.
        unsafe {
            let _ = SetWindowPos(
                HWND(h.hwnd.get() as *mut _),
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// X11 window managers keep `_NET_WM_STATE_BELOW` by themselves.
#[cfg(not(windows))]
fn keep_at_bottom(_ctx: &egui::Context, _frame: &eframe::Frame) {}

/// The short video that goes with an iPhone Live Photo: same name, `.MOV`, same folder.
fn live_video(photo: &Path) -> Option<PathBuf> {
    let ext = photo.extension()?.to_string_lossy().to_lowercase();
    if !matches!(ext.as_str(), "heic" | "heif" | "jpg" | "jpeg") {
        return None;
    }
    ["MOV", "mov"].iter().map(|e| photo.with_extension(e)).find(|p| p.is_file())
}

/// `photo`'s name with the video's extension, e.g. `IMG_1 (1).HEIC` -> `IMG_1 (1).MOV`.
fn with_video_ext(photo: &Path, video: &Path) -> PathBuf {
    photo.with_extension(video.extension().unwrap_or_default())
}

/// `base`, or `name (1).ext`, `name (2).ext`... if that is taken (also checking the video's name).
fn free_name(base: &Path, video: Option<&Path>) -> PathBuf {
    let free = |c: &Path| !c.exists() && video.is_none_or(|v| !with_video_ext(c, v).exists());
    if free(base) {
        return base.to_path_buf();
    }
    let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = base.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = base.parent().unwrap_or(Path::new("."));
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| free(c))
        .expect("an unused name exists")
}

/// Renames, or copies and deletes when the folders are on different drives.
/// Never leaves two copies behind: if the original cannot be deleted, the copy is removed again.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
        other => return other,
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from).inspect_err(|_| {
        let _ = std::fs::remove_file(to);
    })
}

/// Wayland compositors (GNOME, KDE) have no "stay below other windows" level, so there the
/// frame opens as an ordinary window instead of trying to sit on the desktop.
fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland") || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn parse_args(st: &Settings) -> Config {
    let mut dirs = Vec::new();
    let mut interval = st.interval;
    let mut normal_level = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--interval" | "-i" => {
                if let Some(v) = args.next().and_then(|v| v.parse().ok()) {
                    interval = v;
                }
            }
            "--normal" => normal_level = true,
            "--help" | "-h" => {
                println!(
                    "frame [--interval SECONDS] [--normal] [FOLDER...]\n\nDefault folder: ~/Pictures, default interval: 10s.\n--normal keeps the window at normal stacking level instead of below other windows (always the case on Wayland)."
                );
                std::process::exit(0);
            }
            flag if flag.starts_with('-') => {
                eprintln!("frame: unknown option {flag} (see --help)");
                std::process::exit(2);
            }
            // Absolute, so Undo can find the photo in the Trash (which records full paths)
            // and excluded folders (picked as full paths) match.
            _ => dirs.push(std::path::absolute(&a).unwrap_or_else(|_| PathBuf::from(&a))),
        }
    }
    if dirs.is_empty() {
        dirs = st.dirs.clone();
    }
    if dirs.is_empty() {
        dirs.push(home_dir().map(|h| h.join("Pictures")).unwrap_or_else(|| PathBuf::from(".")));
    }
    let normal_level = normal_level || is_wayland();
    Config { dirs, interval: Duration::from_secs(interval.max(1)), normal_level, settings: st.clone() }
}

/// Opens the frame window with the given renderer. Returns once the window is closed.
fn run(renderer: eframe::Renderer) -> eframe::Result {
    let st = settings::load();
    let cfg = parse_args(&st);
    let level = if cfg.normal_level { egui::WindowLevel::Normal } else { egui::WindowLevel::AlwaysOnBottom };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Frame")
        .with_app_id("frame")
        .with_inner_size([520.0, 360.0])
        .with_min_inner_size([200.0, 160.0])
        .with_decorations(false)
        .with_taskbar(false)
        .with_window_level(level);
    if let Ok(icon) = image::load_from_memory(include_bytes!("../assets/icon-256.png")) {
        let icon = icon.to_rgba8();
        let (width, height) = icon.dimensions();
        viewport = viewport.with_icon(egui::IconData { rgba: icon.into_raw(), width, height });
    }
    // Reopen where it was left; ignore values that cannot be a real window.
    if let Some([x, y, w, h]) = cfg.settings.window
        && (200.0..=10000.0).contains(&w)
        && (160.0..=10000.0).contains(&h)
    {
        viewport = viewport.with_inner_size([w, h]);
        if (-1000.0..=20000.0).contains(&x) && (-1000.0..=20000.0).contains(&y) {
            viewport = viewport.with_position([x, y]);
        }
    }
    let options = eframe::NativeOptions { viewport, renderer, ..Default::default() };
    let mut cfg = Some(cfg);
    eframe::run_native(
        "frame",
        options,
        Box::new(move |cc| {
            fonts::add_cjk_fallback(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx, cfg.take().expect("single init"))))
        }),
    )
}

/// Linux uses OpenGL. Windows uses Direct3D through wgpu first: many Windows machines (virtual
/// machines, remote sessions, basic display drivers) have no OpenGL 2, while Direct3D always has
/// Windows' own software renderer to fall back on. OpenGL is the second choice there.
fn main() -> eframe::Result {
    #[cfg(windows)]
    let result = run(eframe::Renderer::Wgpu).or_else(|e| {
        eprintln!("frame: Direct3D failed ({e}), trying OpenGL");
        run(eframe::Renderer::Glow)
    });
    #[cfg(not(windows))]
    let result = run(eframe::Renderer::Glow);
    if let Err(e) = &result {
        // A Windows GUI program has no console to print to, so say it in a message box.
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Frame could not start")
            .set_description(format!("Frame could not open its window:\n{e}"))
            .show();
    }
    result
}
