fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("ProductName", "lanlink")
        .set("FileDescription", "lanlink")
        .set("OriginalFilename", "lanlink.exe")
        .set("LegalCopyright", "MIT License");
    res.compile().expect("failed to embed Windows resources");
}
