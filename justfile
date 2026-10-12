test_web_chrome:
    # Runs compress_image_js (Canvas-based WebP encoding) in Chrome.
    # Prerequisites (one-time setup):
    #   cargo install wasm-pack
    #   Download chromedriver matching your Chrome version to ~/bin/chromedriver:
    #     curl -sO "https://storage.googleapis.com/chrome-for-testing-public/$(google-chrome --version | awk '{print $3}')/mac-arm64/chromedriver-mac-arm64.zip"
    #     unzip chromedriver-mac-arm64.zip && mv chromedriver-mac-arm64/chromedriver ~/bin/chromedriver
    # Note: ~/bin/chromedriver must match your Chrome version; brew's chromedriver may differ.
    PATH=$PATH:~/bin wasm-pack test --headless --chrome --test compress_images_wasm_browser -- --nocapture

test_web_safari:
    PATH=$PATH:~/bin wasm-pack test --headless --safari --test compress_images_wasm_browser -- --nocapture

test_web_chrome_zenwebp:
    # Same browser tests with the pure-Rust zenwebp encoder (wasm ~+419 KiB).
    # --features must come before `--` (args after `--` go to the wasm-bindgen
    # test runner); --release because debug wasm zenwebp encoding is very slow.
    # The GIF test asserts an animated WebP (ANIM chunk), which requires a
    # browser with the WebCodecs ImageDecoder API (Chrome) — hence Chrome here.
    PATH=$PATH:~/bin wasm-pack test --release --headless --chrome --test compress_images_wasm_browser --features web-zenwebp -- --nocapture

test_ios:
    rustup target add aarch64-apple-ios-sim
    cargo install cargo-dinghy
    # xcrun simctl boot "iPhone 17 Pro"
    open -a Simulator
    cargo dinghy --platform auto-ios-aarch64-sim --device sim test --test compress_images -- compress_all_test_images --nocapture

test_android_24:
    # Prerequisites (one-time setup):
    #   rustup target add aarch64-linux-android
    #   cargo install cargo-dinghy
    #   Create an AVD in Android Studio (API 24+, arm64-v8a) and start it,
    #   or connect a physical device with USB debugging enabled.
    cargo dinghy --platform auto-android-aarch64-api24 test --test compress_images -- compress_all_test_images --nocapture

test_android_31:
    cargo dinghy --platform auto-android-aarch64-api31 test --test compress_images -- compress_all_test_images --nocapture

test_desktop:
    cargo test --test compress_images -- --nocapture

test_all:
    just test_web_chrome
    just test_web_safari
    just test_ios
    just test_android_24
    just test_android_31
    just test_desktop

build_android:
    cargo dinghy --platform auto-android-aarch64-api24 build --release

build_web_canvas:
    # Default: browser Canvas encoder (~32 KiB wasm).
    wasm-pack build --release

build_web_zenwebp:
    # Pure-Rust zenwebp encoder (~450 KiB wasm; see README).
    wasm-pack build --release -- --features web-zenwebp
