//! JNI entry points for `android-test-harness`. Built only with the `harness`
//! feature so the shipping library exposes no extra symbols.

use std::ffi::{c_char, CString};
use std::path::Path;

use jni::objects::{JObject, JString};
use jni::sys::{jfloat, jint};
use jni::JNIEnv;

use crate::{compress_video_file, VideoCompressOptions};

extern "C" {
    fn __android_log_print(prio: i32, tag: *const c_char, fmt: *const c_char, ...) -> i32;
}

fn log(message: &str) {
    let Ok(message) = CString::new(message) else {
        return;
    };
    unsafe {
        __android_log_print(
            6,
            c"media_compress".as_ptr(),
            c"%s".as_ptr(),
            message.as_ptr(),
        );
    }
}

fn jstring(env: &mut JNIEnv, value: &JString) -> Option<String> {
    env.get_string(value).ok().map(Into::into)
}

/// Returns 1 when the re-encode was written (even if larger than the source),
/// and -1 on error.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_com_hyzmm_mediacompress_testapp_NativeBridge_compressVideoFile(
    mut env: JNIEnv,
    _this: JObject,
    input: JString,
    output: JString,
    crf: jfloat,
    max_width: jint,
    max_height: jint,
) -> jint {
    let (Some(input), Some(output)) = (jstring(&mut env, &input), jstring(&mut env, &output))
    else {
        log("compressVideoFile: invalid path arguments");
        return -1;
    };

    let options = VideoCompressOptions {
        crf,
        max_width: (max_width > 0).then_some(max_width as u32),
        max_height: (max_height > 0).then_some(max_height as u32),
    };

    log(&format!(
        "compressVideoFile: {input} -> {output} crf {crf} max {max_width}x{max_height}"
    ));

    match compress_video_file(Path::new(&input), Path::new(&output), options) {
        Ok(()) => 1,
        Err(error) => {
            log(&format!("compressVideoFile failed: {error}"));
            -1
        }
    }
}
