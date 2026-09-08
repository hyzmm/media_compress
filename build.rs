fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match os.as_str() {
        "macos" | "ios" => {
            println!("cargo:rustc-link-lib=framework=ImageIO");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
            println!("cargo:rustc-link-lib=framework=CoreMedia");
            println!("cargo:rustc-link-lib=framework=CoreVideo");
            println!("cargo:rustc-link-lib=framework=AVFoundation");
        }
        "android" => {
            println!("cargo:rustc-link-lib=jnigraphics");
            println!("cargo:rustc-link-lib=mediandk");
            // Disable packed relocations (DT_ANDROID_RELR) so binaries
            // compiled for api30+ can still run on older Android versions.
            println!("cargo:rustc-link-arg=-Wl,--pack-dyn-relocs=none");
        }
        "windows" => {
            println!("cargo:rustc-link-lib=windowscodecs");
            println!("cargo:rustc-link-lib=ole32");
            println!("cargo:rustc-link-lib=mfplat");
            println!("cargo:rustc-link-lib=mfreadwrite");
        }
        _ => {}
    }
}
