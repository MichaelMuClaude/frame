# frame

A small desktop photo frame. It shows random photos from your folders inside a resizable
picture frame, and lets you send the one you are looking at to the Trash, or move it to another
folder, with a single click.

Written in Rust with [egui](https://github.com/emilk/egui). Works with JPG, PNG, WebP, GIF, BMP,
TIFF and iPhone HEIC/HEIF photos.

## What it does

- Shows a random photo and changes it every 10 seconds (configurable). The slideshow waits
  while the mouse is over the photo.
- The frame reshapes itself to each photo, so portrait and landscape phone photos both fill it.
- Controls appear only while the mouse is over the photo, so the rest of the time it is just a
  clean picture frame:
  - **Trash** (top right): moves the photo to the system Trash. An **Undo** button stays for 8 seconds.
  - **Move** (top left): moves the photo to a folder you pick. Recent folders are listed; **Undo** works here too.
  - For an iPhone Live Photo (a `.HEIC` or `.JPG` with a same-named `.MOV` next to it), Trash and Move take the video along and Undo brings both back. Only the still photo is shown; videos are not played.
  - **Previous** (left edge): goes back to the photo you just saw.
  - **Show in folder** (bottom right, also in the right-click menu): opens the file manager with the photo selected (Explorer on Windows, Finder on macOS, and on Linux any file manager that supports the freedesktop `ShowItems` call, otherwise it just opens the folder).
- Drag the photo to move the window, drag the wooden edges or corners to resize it.
- Right-click for next/previous, pause, quit, and **Settings...** (photo folders, folders to
  exclude, interval, whether the frame follows each photo's shape). An excluded folder and
  everything below it is left out of the slideshow; **Include again** brings it back. Settings are saved to
  `~/.config/frame/settings.txt` (`%APPDATA%\frame\settings.txt` on Windows).
- Keyboard: Left/Right arrow for previous/next, Space to pause (a "Paused" label shows on hover), Delete to trash,
  Ctrl+Z to undo the last Trash or Move.
- The window reopens where you left it, at the same size.
- File names in Chinese, Japanese or Korean are drawn with a font from your system (Noto Sans CJK,
  Microsoft YaHei, PingFang...), since egui's built-in fonts do not have those characters.
- On X11 the window sits below all other windows, like a desktop widget. On Wayland there is no
  such window level, so it opens as an ordinary window. `--normal` forces that everywhere.

## Usage

```
frame [--interval SECONDS] [--normal] [FOLDER...]
```

With no folders it uses the ones from Settings, or `~/Pictures` the first time.

## Building

You need a recent Rust toolchain and the libheif development files (version 1.17 or newer).

```
# Debian / Ubuntu
sudo apt install libheif-dev
cargo build --release
```

The binary is `target/release/frame`.

HEIC support is the `heic` cargo feature (on by default). To build without libheif, for example for
Windows, use `cargo build --release --no-default-features`; HEIC/HEIF photos are then not listed.

## Status

Tested on Linux with an X11 session. The Wayland behaviour and the Windows build have not been
tested yet. On Windows, libheif has to be provided to the build (for example through vcpkg).

Deleting and moving act on your real files. Delete goes to the Trash, and both have an Undo, but
try it on a copy of a folder first.

## License

MIT, see [LICENSE](LICENSE).
