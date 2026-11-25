//! The desktop helper's KMS capture on this PC (run as root): each lit screen, what its framebuffer is, and its
//! picture saved as a PPM file. Usage: kms_probe [output folder] [gpu] (default: the current folder; gpu: every buffer
//! read through the GPU, as a tiled one is); kms_probe gpu-self-test [card]: the GPU reads a picture of its own layout.

#[cfg(target_os = "linux")]
fn main() {
    use std::io::Write;
    if std::env::args().nth(1).as_deref() == Some("gpu-self-test") {
        match queqiao_capture::kms::gpu_self_test(std::env::args().nth(2).as_deref()) {
            Ok(found) => println!(
                "{found}
GPU_SELF_TEST_PASS"
            ),
            Err(error) => println!("GPU_SELF_TEST_FAIL {error}"),
        }
        return;
    }
    let folder = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let gpu = std::env::args().nth(2).as_deref() == Some("gpu");
    let mut kms = match queqiao_capture::kms::Kms::open() {
        Ok(kms) => kms,
        Err(error) => {
            println!("KMS_FAIL {error}");
            std::process::exit(1);
        }
    };
    kms.force_gpu(gpu);
    let screens = kms.screens().unwrap_or_default();
    println!("screens: {}", screens.len());
    let mut failed = screens.is_empty();
    for (index, screen) in screens.iter().enumerate() {
        println!(
            "{}",
            kms.describe(screen)
                .unwrap_or_else(|e| format!("describe: {e}"))
        );
        println!("{}", kms.describe_pointer(screen));
        if gpu {
            println!(
                "GPU reader: {}",
                kms.gpu_reader(screen.card).unwrap_or_else(|e| e)
            );
        }
        let started = std::time::Instant::now();
        match kms.capture(screen) {
            Ok(image) => {
                let lit = image
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[0] > 16 || p[1] > 16 || p[2] > 16)
                    .count();
                println!(
                    "screen {index}: {}x{} read in {:.1} ms, {lit} pixels not black; frame id {:?}",
                    image.width,
                    image.height,
                    started.elapsed().as_secs_f64() * 1000.0,
                    kms.frame_id(screen)
                );
                let path = folder.join(format!("kms_{index}.ppm"));
                let mut out = Vec::with_capacity(image.pixels.len());
                write!(out, "P6\n{} {}\n255\n", image.width, image.height).unwrap();
                for p in image.pixels.as_chunks::<4>().0 {
                    out.extend_from_slice(&[p[2], p[1], p[0]]);
                }
                std::fs::write(&path, out).unwrap();
                println!("saved {}", path.display());
            }
            Err(error) => {
                println!("screen {index}: {error}");
                failed = true;
            }
        }
    }
    println!("{}", if failed { "KMS_FAIL" } else { "KMS_PASS" });
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("kms_probe: Linux only");
}
