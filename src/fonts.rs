//! egui's built-in fonts have no Chinese, Japanese or Korean characters, so file names in those
//! scripts would show as boxes. This adds a font from the system as a fallback, if one is found.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

pub fn add_cjk_fallback(ctx: &egui::Context) {
    let Some((bytes, index)) = find_cjk_font() else { return };
    let mut fonts = FontDefinitions::default();
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    fonts.font_data.insert("system-cjk".into(), Arc::new(data));
    // Last in each family: the built-in fonts still draw Latin text, this only fills the gaps.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("system-cjk".into());
    }
    ctx.set_fonts(fonts);
}

/// (font file contents, face index inside a .ttc collection)
fn find_cjk_font() -> Option<(Vec<u8>, u32)> {
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(found) = fontconfig_match() {
        return Some(found);
    }
    // Known locations, Simplified Chinese faces first.
    let candidates: &[(&str, u32)] = &[
        ("C:\\Windows\\Fonts\\msyh.ttc", 0),
        ("C:\\Windows\\Fonts\\simsun.ttc", 0),
        ("/System/Library/Fonts/PingFang.ttc", 0),
        ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
    ];
    candidates.iter().find_map(|(path, index)| std::fs::read(path).ok().map(|b| (b, *index)))
}

/// Asks fontconfig for the system's preferred Chinese sans-serif face.
#[cfg(all(unix, not(target_os = "macos")))]
fn fontconfig_match() -> Option<(Vec<u8>, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", "sans:lang=zh-cn"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let file = lines.next()?;
    let index = lines.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    // fc-match always answers with something; only use it if it really is a CJK font.
    let lower = file.to_lowercase();
    if !["cjk", "wqy", "droidsansfallback", "hei", "song", "kai", "ming", "sarasa", "source han"]
        .iter()
        .any(|k| lower.contains(k))
    {
        return None;
    }
    Some((std::fs::read(file).ok()?, index))
}
