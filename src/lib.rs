pub mod compress;
pub mod error;

#[cfg(target_os = "android")]
mod android_runtime;

pub use compress::image::{compress_image, CompressOptions, ImageFormat};
pub use error::Error;

#[cfg(target_os = "android")]
pub fn init_android_java_vm(vm: *mut jni::sys::JavaVM) {
    android_runtime::init_java_vm(vm);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    init_android_java_vm(vm);
    jni::sys::JNI_VERSION_1_6
}

/// On the Web/WASM platform, use this async function instead of `compress_image`.
#[cfg(target_arch = "wasm32")]
pub use compress::image::wasm::compress_image_js;
