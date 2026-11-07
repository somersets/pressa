fn main() {
    println!("cargo:rerun-if-changed=../../assets/pressa.ico");
    // Embed the icon and version info into Pressa.exe.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/pressa.ico")
            .set("ProductName", "Pressa")
            .set("FileDescription", "Pressa image converter")
            .set("LegalCopyright", "MIT License");
        res.compile().expect("failed to embed Windows resources");
    }
}
