//! Converting a folder must not leave anything inside it open, so the user can delete it afterwards.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use pressa_core::{ConvertOptions, collect_inputs, convert_batch};

#[test]
fn source_folder_can_be_renamed_after_conversion() {
    let base = std::env::temp_dir().join(format!("pressa-handles-{}", std::process::id()));
    let src = base.join("Album");
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 128]));
    img.save(src.join("a.png")).unwrap();
    img.save(sub.join("b.jpg")).unwrap();
    if let Ok(video) = std::env::var("PRESSA_TEST_VIDEO") {
        std::fs::copy(video, sub.join("clip.mp4")).unwrap();
    }

    let out = base.join("out");
    let opts = ConvertOptions { output_dir: Some(out.clone()), ..Default::default() };
    let inputs = collect_inputs(&[src.clone()], true);
    let failures = std::sync::Mutex::new(Vec::new());
    convert_batch(&inputs, &opts, &AtomicBool::new(false), |_, _| {}, |i, r| {
        if let Err(e) = r {
            failures.lock().unwrap().push(format!("{}: {e:#}", inputs[i].path.display()));
        }
    });
    assert!(failures.lock().unwrap().is_empty(), "{:?}", failures.lock().unwrap());

    // On Windows, renaming fails while any file or folder inside is open.
    let renamed: PathBuf = base.join("Album-renamed");
    std::fs::rename(&src, &renamed).expect("source folder is still in use");
    std::fs::remove_dir_all(&base).unwrap();
}
