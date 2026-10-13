# media_compress

跨平台图片/视频压缩库（Rust）。图片：统一输出为有损 WebP——多帧 GIF 输出动画 WebP（帧延迟原样保留；Web 端在 `web-zenwebp` 构建下），其他动图输出首帧；视频：输出 H.264 + MP4（AAC 音频）。

## 实现方案

### 1. 总体架构

- 入口 API：`compress_image(input, options)`
- Web 专用异步 API：`compress_image_js(input, quality, min_width?, min_height?)`
- 编码目标：
    - Native 平台：统一编码为 WebP；多帧 GIF 编码为动画 WebP（帧延迟按源原样保留，不合并、不调整）
    - Web/WASM：默认构建用 `OffscreenCanvas.convertToBlob` 输出 `image/webp`（动图出首帧）；`web-zenwebp` 构建下多帧 GIF 经浏览器 WebCodecs `ImageDecoder` 解帧后由 zenwebp 编码为动画 WebP
- 格式识别：通过文件魔数自动检测（JPEG/PNG/GIF/BMP/WebP/TIFF/HEIC）
- WebP 输入：直接报错（`UnsupportedFormat`），不做重编码——WebP 已是本库输出格式，二次有损重编只会掉画质
- 结果保护：对可能已较优压缩的输入（JPEG/PNG/GIF），若压缩后更大则返回原图字节

### 2. 平台解码与编码路径

- macOS / iOS
    - 解码：ImageIO + CoreGraphics
    - 编码：libwebp（多线程）

- Windows
    - 解码：WIC（Windows Imaging Component）
    - 编码：libwebp

- Android
    - 首选：AImageDecoder（动态加载，API 30+）
    - 回退：JNI BitmapFactory（用于低版本或 AImageDecoder 不可用场景）。JNI 回退链路依赖可用的 `JavaVM`，宿主需在库加载时完成初始化（见下方"Android 集成与初始化"）
    - 编码：libwebp

- Web / WASM
    - 解码：`createImageBitmap`
    - 编码：`OffscreenCanvas.convertToBlob`，输出 `image/webp`
    - 注意：WASM 平台应使用异步 `compress_image_js`，同步 `compress_image` 在 WASM 下会返回不支持

### 3. 压缩策略

- 质量参数：`quality` 取值 0-100（常用 75）
- 尺寸下限：`min_width` / `min_height` 可选；压缩时会按比例缩小并满足下限，不会放大小图
- Native 编码配置：有损 WebP + 多线程
- GIF：多帧 GIF 输出动画 WebP，帧延迟按源原样保留（不合并、不调整）；单帧 GIF 输出静止 WebP；Android API < 31 因系统动画解码接口缺失，GIF 出首帧
- 动图处理：非 GIF 动图（APNG）输入导出 WebP 首帧（poster frame）

## 各平台图片格式支持情况

说明：下面是“按当前实现链路可达能力”的支持矩阵。浏览器和 Android 厂商实现可能导致个别机型差异。

| 平台                             | 输入格式支持                                                                 | 输出格式                  |
| -------------------------------- | ---------------------------------------------------------------------------- | ------------------------- |
| macOS / iOS                      | JPEG, PNG, GIF（多帧→动画 WebP）, BMP, HEIC, TIFF                            | WebP / 动画 WebP          |
| Windows                          | JPEG, PNG, GIF（多帧→动画 WebP，WIC 能力范围内）, BMP, TIFF                  | WebP / 动画 WebP          |
| Android (API 31+, AImageDecoder) | JPEG, PNG, GIF（多帧→动画 WebP）, BMP，及设备支持的其他系统解码格式          | WebP / 动画 WebP          |
| Android (API < 31 或回退 JNI)    | JPEG, PNG, GIF（首帧）, BMP（HEIC/TIFF 不支持）                              | WebP                      |
| Web/WASM（浏览器）               | 由 `createImageBitmap` 决定，常见为 JPEG/PNG/GIF/BMP（AVIF 等视浏览器）；GIF 动画见下 | WebP（`web-zenwebp` 构建下 GIF 可输出动画 WebP） |

WebP 输入不在上表之列：所有平台在魔数检测命中 WebP 后直接返回 `UnsupportedFormat` 错误，不进入解码/编码（原因见「总体架构」）。

Web 端 GIF：
- 默认 canvas 构建：输出首帧静止 WebP；
- `web-zenwebp` 构建：浏览器支持 WebCodecs `ImageDecoder`（Chrome 94+ / Safari 16.4+ / Firefox 133+）时输出动画 WebP（帧延迟原样保留），否则回退首帧。

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

| 格式 |   q50 |   q75 |    q90 |
| ---- | ----: | ----: | -----: |
| BMP  |  1.2% |  1.7% |   3.8% |
| GIF  |  8.6% | 11.9% |  29.4% |
| HEIC | 14.3% | 26.9% |  78.3% |
| JPEG | 38.9% | 57.4% | 100.0% |
| PNG  |  7.2% | 10.8% |  27.7% |
| TIFF |  6.1% |  9.1% |  23.4% |
| WebP | 25.7% | 39.2% |  89.9% |

数据解读：

- 对 BMP、TIFF、PNG 这类源文件，压缩收益通常很高。
- 对已压缩格式（JPEG），收益取决于原图质量与编码参数。
- “加权节省率”受超大 BMP 样本影响明显，建议结合业务真实分布评估。
- 在当前样本上，quality 越低压缩率越高，quality=50 相比 90 节省率提升明显。
- 建议线上默认值从 75 起步，再按画质目标向 50 或 90 调整。
- 对 JPEG 这类已压缩格式，q90 接近无收益；对 PNG/BMP/TIFF，q75 仍有较高节省率。
- 本批数据采集于 WebP 输入仍会被压缩的旧版本；当前版本对 WebP 输入直接报错，上表中 WebP 行仅作历史参考。

## 快速使用

### Rust (Native)

```rust
use media_compress::compress_image;
use media_compress::CompressOptions;

let input = std::fs::read("input.png")?;
let options = CompressOptions {
    quality: 75.0,
    min_width: Some(1280),
    min_height: Some(720),
};
let out = compress_image(&input, options)?;
std::fs::write("out.webp", out)?;
```

### WebAssembly (Browser)

```javascript
import init, { compress_image_js } from './pkg/media_compress.js';

await init();
const out = await compress_image_js(inputBytes, 75, 1280, 720);
// out 是 Uint8Array，输出为 WebP
// （web-zenwebp 构建 + 浏览器支持 ImageDecoder 时，GIF 输入输出动画 WebP）
```

## 视频压缩（macOS / iOS / Android / Windows）

- macOS / iOS：通过 AVFoundation（AVAssetReader/AVAssetWriter）转码。
- Android：通过 NDK 原生 MediaCodec + MediaMuxer（libmediandk）转码，无需调用方注入 `JavaVM`。
- Windows：通过 Media Foundation（IMFSourceReader / IMFSinkWriter）转码，视频解码为 RGB32、缩放后编码为 H.264，音频重编码为 AAC。

统一输出 H.264 + MP4，音频重编码为 AAC。压缩参数为目标码率 + 分辨率上限，只缩小不放大、保持宽高比；若压缩后更大则返回原始字节。

```rust
use media_compress::{compress_video, VideoCompressOptions};

let input = std::fs::read("input.mp4")?;
let mut options = VideoCompressOptions::new(2000); // 目标码率 2000 kbps
options.max_width = Some(1280);
options.max_height = Some(720);
let out = compress_video(&input, options)?;
std::fs::write("out.mp4", out)?;
```

- 支持的源容器：MP4 / MOV（QuickTime）等平台可解码格式。
- 竖屏/旋转视频的旋正暂未处理，作为后续项。

## Android 集成与初始化

本库在 Android 平台使用两条解码链路：

### 主链路：AImageDecoder（API 30+）

- **动画支持**：多帧 GIF 输出动画 WebP（帧延迟原样保留；系统动画接口需 API 31+，API 30 上 GIF 出首帧）；其余多帧格式输出首帧
- **API 兼容性**：动态加载以支持低版本 Android 系统

### 回退链路：JNI BitmapFactory（API < 28）

如果 AImageDecoder 不可用或解码失败，库会自动回退至 JNI BitmapFactory。该链路需要宿主在启动时注入 `JavaVM`，否则会出现错误：

`Platform not supported for format: Android JNI fallback decoder unavailable: JavaVM was not initialized...`

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

- 本库图片压缩统一输出 WebP（多帧 GIF 输出动画 WebP，帧延迟原样保留；其余动图取首帧）；视频压缩输出 H.264/MP4（macOS / iOS / Android / Windows，见下方「视频压缩」）。
- 如需更稳定的“业务压缩率基线”，建议在你们真实样本集上固定 `quality` 进行批量评测并记录 P50/P90。
