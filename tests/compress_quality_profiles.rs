use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use media_compress::{compress_image, ImageFormat};

fn project_base_dir() -> PathBuf {
    let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if compile_time.exists() {
        compile_time
    } else {
        std::env::current_dir().expect("cannot determine current directory")
    }
}

fn test_images_dir() -> PathBuf {
    let base = project_base_dir();
    let direct = base.join("test_images");
    if direct.exists() {
        direct
    } else {
        base.join("test_data").join("test_images")
    }
}

#[derive(Default, Clone)]
struct Aggregate {
    count: usize,
    orig_bytes: u64,
    out_bytes: u64,
    sum_ratio_pct: f64,
}

fn format_label(format: &ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        ImageFormat::Gif => "gif",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Webp => "webp",
        ImageFormat::Heic => "heic",
        ImageFormat::Tiff => "tiff",
    }
}

impl Aggregate {
    fn record(&mut self, orig: usize, out: usize) {
        self.count += 1;
        self.orig_bytes += orig as u64;
        self.out_bytes += out as u64;
        self.sum_ratio_pct += out as f64 / orig as f64 * 100.0;
    }

    fn mean_ratio_pct(&self) -> f64 {
        self.sum_ratio_pct / self.count as f64
    }

    fn weighted_ratio_pct(&self) -> f64 {
        self.out_bytes as f64 / self.orig_bytes as f64 * 100.0
    }
}

#[test]
fn compare_quality_profiles() {
    let input_dir = test_images_dir();
    if !input_dir.exists() {
        eprintln!("test_images/ directory does not exist - skipping test");
        return;
    }

    let entries: Vec<_> = fs::read_dir(&input_dir)
        .expect("failed to read test_images/")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    assert!(
        !entries.is_empty(),
        "test_images/ is empty - add some images to test"
    );

    let qualities = [50.0_f32, 75.0_f32, 90.0_f32];
    let mut total_aggs: Vec<Aggregate> = vec![Aggregate::default(); qualities.len()];
    let mut per_format_aggs: BTreeMap<String, Vec<Aggregate>> = BTreeMap::new();

    println!("quality\tcount\tmean_ratio\tmean_saving\tweighted_ratio\tweighted_saving");

    for entry in &entries {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name == ".DS_Store" {
            continue;
        }

        let input =
            fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
        let format = match ImageFormat::detect(&input) {
            Some(f) => f,
            None => continue,
        };

        let key = format_label(&format).to_string();
        let format_aggs = per_format_aggs
            .entry(key)
            .or_insert_with(|| vec![Aggregate::default(); qualities.len()]);

        for (i, quality) in qualities.iter().enumerate() {
            match compress_image(&input, *quality) {
                Ok(out) => {
                    total_aggs[i].record(input.len(), out.len());
                    format_aggs[i].record(input.len(), out.len());
                }
                Err(media_compress::Error::PlatformNotSupported(_)) => {}
                Err(e) => panic!("compress_image failed for {}: {}", path.display(), e),
            }
        }
    }

    for (i, quality) in qualities.iter().enumerate() {
        let agg = &total_aggs[i];

        if agg.count == 0 {
            println!("{quality:.0}\t0\tN/A\tN/A\tN/A\tN/A");
        } else {
            let mean_ratio = agg.mean_ratio_pct();
            let mean_saving = 100.0 - mean_ratio;
            let weighted_ratio = agg.weighted_ratio_pct();
            let weighted_saving = 100.0 - weighted_ratio;

            println!(
                "{quality:.0}\t{}\t{mean_ratio:.1}%\t{mean_saving:.1}%\t{weighted_ratio:.1}%\t{weighted_saving:.1}%",
                agg.count
            );
        }
    }

    println!("\nformat\tq50_ratio\tq75_ratio\tq90_ratio");
    for (format, aggs) in &per_format_aggs {
        let q50 = if aggs[0].count == 0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", aggs[0].mean_ratio_pct())
        };
        let q75 = if aggs[1].count == 0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", aggs[1].mean_ratio_pct())
        };
        let q90 = if aggs[2].count == 0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", aggs[2].mean_ratio_pct())
        };

        println!("{}\t{}\t{}\t{}", format, q50, q75, q90);
    }
}
