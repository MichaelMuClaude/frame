// Embeds the icon and version information into the Windows exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Frame Photo Widget");
        res.set("FileDescription", "Frame Photo Widget");
        res.set("LegalCopyright", "MIT License");
        res.compile().expect("embedding the Windows resources failed");
    }
}
