pub mod compress;
pub mod error;

pub use compress::image::{compress_image, ImageFormat};
pub use error::Error;

/// On the Web/WASM platform, use this async function instead of `compress_image`.
#[cfg(target_arch = "wasm32")]
pub use compress::image::wasm::compress_image_js;
