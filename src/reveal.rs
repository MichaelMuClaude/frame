//! "Show in folder": opens the file manager with a photo selected.

use std::path::Path;
use std::process::Command;
#[cfg(not(windows))]
use std::process::Stdio;

pub fn show_in_folder(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("the file is gone".into());
    }
    imp(path)
}

/// Windows: the shell call Explorer's own "Open file location" uses. `explorer /select,` opens
/// the folder too, but on Windows 11 it often fails to select the file, so it is only a fallback.
#[cfg(windows)]
fn imp(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;

    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the calls; the item list
    // from ILCreateFromPathW is freed exactly once, and COM is only uninitialised if this
    // thread initialised it.
    let selected = unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let pidl = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        let result = if pidl.is_null() {
            Err("the path could not be resolved".to_string())
        } else {
            let r = SHOpenFolderAndSelectItems(pidl, None, 0).map_err(|e| e.to_string());
            ILFree(Some(pidl));
            r
        };
        if com.is_ok() {
            CoUninitialize();
        }
        result
    };
    if selected.is_ok() {
        return Ok(());
    }
    // Explorer wants `/select,"C:\a b\c.jpg"`; raw_arg keeps the quotes exactly like that.
    // It also returns a non-zero exit code on success, so only the launch itself is checked.
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn imp(path: &Path) -> Result<(), String> {
    run(Command::new("open").arg("-R").arg(path))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn imp(path: &Path) -> Result<(), String> {
    // The freedesktop FileManager1 interface (Nautilus, Dolphin, Nemo, Thunar, ...) selects the file.
    let uri = file_uri(path);
    let selected = run(Command::new("dbus-send")
        .args(["--session", "--print-reply", "--dest=org.freedesktop.FileManager1"])
        .args(["/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"])
        .arg(format!("array:string:{uri}"))
        .arg("string:"));
    if selected.is_ok() {
        return Ok(());
    }
    // No such file manager: at least open the folder.
    let dir = path.parent().ok_or("the photo has no folder")?;
    Command::new("xdg-open")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("no file manager found ({e})"))
}

#[cfg(any(target_os = "macos", all(unix, not(target_os = "macos"))))]
fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `file:///home/me/My%20Photos/a.jpg`: everything except unreserved characters and `/` is %-encoded.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn uri_encodes_spaces_unicode_and_quotes() {
        assert_eq!(file_uri(Path::new("/p/My Photos/a#1.jpg")), "file:///p/My%20Photos/a%231.jpg");
        assert_eq!(file_uri(Path::new("/p/照.jpg")), "file:///p/%E7%85%A7.jpg");
    }

    #[test]
    fn missing_file_is_reported() {
        assert!(show_in_folder(Path::new("/definitely/not/here.jpg")).is_err());
    }
}
