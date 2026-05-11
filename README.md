# media_compress

跨平台图片压缩库（Rust），统一输出为有损 WebP（Web/WASM 在不支持 WebP 编码时回退 JPEG）。

## 实现方案

### 1. 总体架构

- 入口 API：`compress_image(input, quality)`
- Web 专用异步 API：`compress_image_js(input, quality)`
- 编码目标：
    - Native 平台：统一编码为 WebP
    - Web/WASM：优先 WebP，浏览器不支持时回退 JPEG
- 格式识别：通过文件魔数自动检测（JPEG/PNG/GIF/BMP/WebP/TIFF/HEIC）
- 结果保护：对可能已较优压缩的输入（WebP/JPEG/PNG），若压缩后更大则返回原图字节

### 2. 平台解码与编码路径

- macOS / iOS
    - 解码：ImageIO + CoreGraphics
    - 动图：逐帧解码并读取帧延迟（GIF 等）

- Windows
    - 解码：WIC（Windows Imaging Component）
    - 动图：读取多帧与元数据延迟，编码为 Animated WebP

- Android
    - 首选：AImageDecoder（动态加载，API 28+）
    - 回退：JNI BitmapFactory（用于低版本或 AImageDecoder 不可用场景）。JNI 回退链路依赖可用的 `JavaVM`，宿主需在库加载时完成初始化（见下方“Android 集成与初始化”）

- Web / WASM
    - 解码：`createImageBitmap`
    - 编码：`OffscreenCanvas.convertToBlob`
    - 输出：优先 `image/webp`，若浏览器不支持则回退 `image/jpeg`
    - 注意：WASM 平台应使用异步 `compress_image_js`，同步 `compress_image` 在 WASM 下会返回不支持

### 3. 压缩策略

- 质量参数：`quality` 取值 0-100（常用 75）
- Native 编码配置：有损 WebP + 多线程
- 动图处理：保留帧序与延迟，输出 Animated WebP

## 各平台图片格式支持情况

说明：下面是“按当前实现链路可达能力”的支持矩阵。浏览器和 Android 厂商实现可能导致个别机型差异。

| 平台                             | 输入格式支持                                                                 | 输出格式                  |
| -------------------------------- | ---------------------------------------------------------------------------- | ------------------------- |
| macOS / iOS                      | JPEG, PNG, GIF, BMP, WebP, HEIC, TIFF                                        | WebP                      |
| Windows                          | JPEG, PNG, GIF, BMP, WebP, TIFF（WIC 能力范围内）                            | WebP                      |
| Android (API 28+, AImageDecoder) | JPEG, PNG, GIF, BMP, WebP，及设备支持的其他系统解码格式（如部分 HEIF/AVIF）  | WebP                      |
| Android (API < 28 或回退 JNI)    | JPEG, PNG, GIF, BMP, WebP（HEIC/TIFF 不支持）                                | WebP                      |
| Web/WASM（浏览器）               | 由 `createImageBitmap` 决定，常见为 JPEG/PNG/GIF/BMP/WebP（AVIF 等视浏览器） | WebP（不支持时回退 JPEG） |

## 大概压缩率数据（样本实测）

测试环境：

- 日期：2026-05-07
- 命令：`cargo test --test compress_images -- compress_all_test_images --nocapture`
- 平台：macOS（当前工作机）
- 参数：`quality = 75`
- 样本目录：`test_images/`

单样本结果（压缩后占原图比例）：

| 文件            |     原始大小 |    压缩后 | 占原图比例 | 节省率 |
| --------------- | -----------: | --------: | ---------: | -----: |
| test_image.bmp  | 47,775,882 B | 830,832 B |       1.7% |  98.3% |
| test_image.gif  |  4,884,001 B | 581,666 B |      11.9% |  88.1% |
| test_image.heic |  2,400,866 B | 645,782 B |      26.9% |  73.1% |
| test_image.jpg  |  1,454,281 B | 834,342 B |      57.4% |  42.6% |
| test_image.png  |  7,842,586 B | 849,206 B |      10.8% |  89.2% |
| test_image.tiff |  9,344,834 B | 854,852 B |       9.1% |  90.9% |
| test_image.webp |  2,109,166 B | 827,026 B |      39.2% |  60.8% |

汇总（同一批样本）：

- 算术平均占比：22.4%（平均节省 77.6%）
- 按字节加权占比：7.2%（加权节省 92.8%）

不同 quality 档位对比（同一批样本，count=7）：

| quality | 算术平均占比 | 算术平均节省率 | 字节加权占比 | 字节加权节省率 |
| ------: | -----------: | -------------: | -----------: | -------------: |
|      50 |        14.6% |          85.4% |         4.7% |          95.3% |
|      75 |        22.4% |          77.6% |         7.2% |          92.8% |
|      90 |        50.3% |          49.7% |        16.9% |          83.1% |

按格式分组的 quality 对比（压缩后占原图比例，越低越好）：

| 格式 | q50 | q75 | q90 |
| --- | --: | --: | --: |
| BMP | 1.2% | 1.7% | 3.8% |
| GIF | 8.6% | 11.9% | 29.4% |
| HEIC | 14.3% | 26.9% | 78.3% |
| JPEG | 38.9% | 57.4% | 100.0% |
| PNG | 7.2% | 10.8% | 27.7% |
| TIFF | 6.1% | 9.1% | 23.4% |
| WebP | 25.7% | 39.2% | 89.9% |

数据解读：

- 对 BMP、TIFF、PNG 这类源文件，压缩收益通常很高。
- 对已压缩格式（JPEG/WebP），收益取决于原图质量与编码参数。
- “加权节省率”受超大 BMP 样本影响明显，建议结合业务真实分布评估。
- 在当前样本上，quality 越低压缩率越高，quality=50 相比 90 节省率提升明显。
- 建议线上默认值从 75 起步，再按画质目标向 50 或 90 调整。
- 对 JPEG/WebP 这类已压缩格式，q90 接近无收益；对 PNG/BMP/TIFF，q75 仍有较高节省率。

## 快速使用

### Rust (Native)

```rust
use media_compress::compress_image;

let input = std::fs::read("input.png")?;
let out = compress_image(&input, 75.0)?;
std::fs::write("out.webp", out)?;
```

### WebAssembly (Browser)

```javascript
import init, { compress_image_js } from './pkg/media_compress.js';

await init();
const out = await compress_image_js(inputBytes, 75);
// out 是 Uint8Array，通常为 WebP；Safari 等可能回退为 JPEG
```

## Android 集成与初始化

从本次更新开始，Android 的 JNI BitmapFactory 回退链路会优先使用宿主注入的 `JavaVM`。
如果宿主未触发 JNI 初始化，可能出现如下错误：

`Platform not supported for format: Android JNI fallback decoder unavailable: JavaVM was not initialized...`

### 为什么会这样

- Flutter App 有 `MainActivity` 并不等于 Rust 库一定拿到了 `JavaVM`。
- 若是通过 Dart FFI 打开动态库，`JNI_OnLoad` 未必自动触发。
- 未注入 `JavaVM` 时，JNI fallback 无法拿到 `JNIEnv`，从而解码失败。

### 宿主接入要求

1. 在宿主 Rust 库提供并导出 `JNI_OnLoad`，并在其中调用：

```rust
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    media_compress::init_android_java_vm(vm);
    jni::sys::JNI_VERSION_1_6
}
```

2. 在 Android 启动阶段显式加载 native 库，确保 `JNI_OnLoad` 被调用（示例：`MainActivity` 的 `companion object` 中调用 `System.loadLibrary("rust_lib_app")`）。

### 排查清单

1. 确认 `JNI_OnLoad` 已编译进当前 Android 产物。
2. 确认 `System.loadLibrary(...)` 的库名与实际 so 名称一致。
3. 确认完整冷启动后复测（仅热重载可能不会重新触发 JNI 初始化）。
4. 若仍失败，优先检查首次解码错误（AImageDecoder）与 fallback 错误是否被上层日志覆盖。

## 测试命令

- 桌面：`just test_desktop`
- 质量档位对比（q=50/75/90）：`cargo test --test compress_quality_profiles -- --nocapture`
- Web Chrome：`just test_web_chrome`
- Web Safari：`just test_web_safari`
- iOS 模拟器：`just test_ios`
- Android：`just test_android_24` / `just test_android_31`

## 备注

- 本库当前聚焦图片压缩（输出 WebP/JPEG 回退），未包含视频压缩链路。
- 如需更稳定的“业务压缩率基线”，建议在你们真实样本集上固定 `quality` 进行批量评测并记录 P50/P90。
