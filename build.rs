fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match os.as_str() {
        "macos" | "ios" => {
            println!("cargo:rustc-link-lib=framework=ImageIO");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
        }
        "android" => {
            println!("cargo:rustc-link-lib=jnigraphics");
        }
        "windows" => {
            println!("cargo:rustc-link-lib=windowscodecs");
            println!("cargo:rustc-link-lib=ole32");
        }
        _ => {}
    }
}
