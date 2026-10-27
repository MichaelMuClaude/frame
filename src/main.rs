//! A small, resizable desktop photo frame: shows random photos from a folder
//! and lets you send the current one to the Trash by hovering and clicking.

mod loader;
mod settings;

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

/// What the Undo button in the toast can reverse.
enum Undo {
    Trashed(PathBuf),
    Moved { from: PathBuf, to: PathBuf },
}

/// What a folder-picker dialog was opened for.
enum Pick {
    MoveTo(PathBuf),
    AddSource,
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
}

impl App {
    fn new(ctx: &egui::Context, cfg: Config) -> Self {
        let scan_rx = loader::spawn_scan(ctx.clone(), cfg.dirs.clone());
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
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(outer.center() - size / 2.0));
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
        if let Some(res) = picked {
            if let Some((kind, _)) = self.pick.take() {
                if let Some(dir) = res {
                    match kind {
                        Pick::MoveTo(photo) => self.move_current_to(photo, dir),
                        Pick::AddSource => {
                            if !self.dirs.contains(&dir) {
                                self.dirs.push(dir);
                                self.save_dirs(ctx);
                            }
                        }
                    }
                }
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
                    self.pending.remove(&path);
                    self.remove_from_queue(&path);
                }
            }
        }
        self.prefetch();
        if let Some(p) = self.current_path().cloned() {
            let already = self.shown.as_ref().is_some_and(|(sp, _)| *sp == p);
            if !already {
                if let Some(tex) = self.cache.get(&p) {
                    self.fit_aspect = Some(tex.aspect_ratio());
                    self.shown = Some((p, tex.clone()));
                    self.shown_at = Instant::now();
                }
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
            if let Some((p, _)) = &self.shown {
                if self.history.last() != Some(p) {
                    self.history.push(p.clone());
                    if self.history.len() > 100 {
                        self.history.remove(0);
                    }
                }
            }
            self.pos += 1;
            if self.pos >= n {
                self.pos = 0;
                self.queue.shuffle(&mut rand::rng());
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
        match trash::delete(&path) {
            Ok(()) => {
                self.shown = None;
                self.cache.remove(&path);
                self.remove_from_queue(&path);
                self.last_action = Some(Undo::Trashed(path));
                self.toast = Some(("Moved to Trash".into(), Instant::now(), true));
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
            Some(Undo::Trashed(path)) => self.undo_trash(path),
            Some(Undo::Moved { from, to }) => {
                let dest = if from.exists() { unique_path(&from) } else { from };
                match move_file(&to, &dest) {
                    Ok(()) => {
                        self.reinsert(dest);
                        "Moved back".to_string()
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
    fn undo_trash(&mut self, target: PathBuf) -> String {
        match trash::os_limited::list() {
            Ok(items) => {
                let mut found: Vec<_> = items.into_iter().filter(|i| i.original_path() == target).collect();
                found.sort_by_key(|i| i.time_deleted);
                match found.pop() {
                    Some(item) => match trash::os_limited::restore_all([item]) {
                        Ok(()) => {
                            self.reinsert(target);
                            "Restored".to_string()
                        }
                        Err(e) => format!("Restore failed: {e}"),
                    },
                    None => "Not found in Trash".to_string(),
                }
            }
            Err(e) => format!("Restore failed: {e}"),
        }
    }

    #[cfg(target_os = "macos")]
    fn undo_trash(&mut self, _target: PathBuf) -> String {
        "Undo not supported here".to_string()
    }

    fn move_current_to(&mut self, photo: PathBuf, dir: PathBuf) {
        let Some(name) = photo.file_name() else { return };
        let mut dest = dir.join(name);
        if dest == photo {
            self.toast = Some(("Already in that folder".into(), Instant::now(), false));
            return;
        }
        if dest.exists() {
            dest = unique_path(&dest);
        }
        match move_file(&photo, &dest) {
            Ok(()) => {
                self.cache.remove(&photo);
                if self.shown.as_ref().is_some_and(|(p, _)| *p == photo) {
                    self.shown = None;
                }
                self.remove_from_queue(&photo);
                self.last_action = Some(Undo::Moved { from: photo, to: dest });
                let label = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.display().to_string());
                self.toast = Some((format!("Moved to {label}"), Instant::now(), true));
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
        let start = self.settings.recent.first().cloned().or_else(home_dir);
        let title = match kind {
            Pick::MoveTo(_) => "Move photo to...",
            Pick::AddSource => "Add a photo folder",
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

    fn rescan(&mut self, ctx: &egui::Context) {
        self.scan_rx = loader::spawn_scan(ctx.clone(), self.dirs.clone());
        self.scanned = false;
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

    fn settings_viewport(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Frame settings")
            .with_inner_size([540.0, 380.0])
            .with_min_inner_size([380.0, 260.0]);
        let dirs = self.dirs.clone();
        let mut secs = self.interval.as_secs();
        let mut fit = self.fit;
        let (mut close, mut add, mut remove) = (false, false, None::<usize>);
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
        if let Some(i) = remove {
            if i < self.dirs.len() && self.dirs.len() > 1 {
                self.dirs.remove(i);
                self.save_dirs(ctx);
            }
        }
        if add {
            self.start_pick(ctx, Pick::AddSource);
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Some X11 window managers drop the level requested at creation, so ask again once mapped.
        self.frames += 1;
        if self.sink && self.frames == 3 {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnBottom));
        }
        self.poll(&ctx);
        if let Some(a) = self.fit_aspect.take() {
            if self.fit {
                // Extremely wide or tall photos keep their letterbox instead of a sliver of a frame.
                self.fit_window(&ctx, a.clamp(0.5, 2.0));
            }
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
                let galley = painter.layout_no_wrap(name, FontId::proportional(12.0), Color32::WHITE);
                let r = Rect::from_min_size(
                    overlay.left_bottom() + vec2(6.0, -galley.size().y - 8.0),
                    galley.size() + vec2(10.0, 6.0),
                );
                painter.rect_filled(r, 4.0, Color32::from_black_alpha(150));
                painter.galley(r.min + vec2(5.0, 3.0), galley, Color32::WHITE);
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
        }

        // Toast with Undo.
        if let Some((msg, at, undoable)) = self.toast.clone() {
            if at.elapsed() > Duration::from_secs(TOAST_SECS) {
                self.toast = None;
            } else {
                let text_w = painter.layout_no_wrap(msg.clone(), FontId::proportional(13.0), Color32::WHITE).size().x;
                let w = (text_w + 28.0 + if undoable { 62.0 } else { 0.0 }).min(photo_area.width());
                let r = Rect::from_center_size(photo_area.center_bottom() + vec2(0.0, -24.0), vec2(w, 32.0));
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

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn unique_path(p: &Path) -> PathBuf {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .expect("an unused name exists")
}

/// Renames, or copies and deletes when the folders are on different drives.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
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
            _ => dirs.push(PathBuf::from(a)),
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

fn main() -> eframe::Result {
    let st = settings::load();
    let cfg = parse_args(&st);
    let level = if cfg.normal_level { egui::WindowLevel::Normal } else { egui::WindowLevel::AlwaysOnBottom };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Frame")
            .with_app_id("frame")
            .with_inner_size([520.0, 360.0])
            .with_min_inner_size([200.0, 160.0])
            .with_decorations(false)
            .with_taskbar(false)
            .with_window_level(level),
        ..Default::default()
    };
    let mut cfg = Some(cfg);
    eframe::run_native(
        "frame",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(&cc.egui_ctx, cfg.take().expect("single init"))))),
    )
}
