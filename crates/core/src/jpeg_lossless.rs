//! Lossless JPEG optimization, like `jpegtran -optimize -progressive`: the compressed
//! DCT coefficients are re-packed with optimal Huffman tables and progressive scans,
//! so every pixel stays exactly the same while the file usually gets 3–10% smaller.

use std::ffi::{c_int, c_uint, c_ulong};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use anyhow::{Result, bail};
use mozjpeg_sys as ffi;

const APP1: c_int = 0xE1;
const APP2: c_int = 0xE2;

/// Re-packs a JPEG without decoding it. Metadata is dropped except the ICC profile
/// (needed for correct colors) and the EXIF orientation (needed for correct rotation).
pub fn optimize(data: &[u8], orientation: u8) -> Result<Vec<u8>> {
    match catch_unwind(AssertUnwindSafe(|| unsafe { transcode(data, orientation) })) {
        Ok(out) => Ok(out),
        Err(e) => {
            let msg = e.downcast_ref::<String>().cloned().unwrap_or_else(|| "libjpeg error".into());
            bail!("lossless JPEG optimization failed: {msg}")
        }
    }
}

extern "C-unwind" fn error_exit(cinfo: &mut ffi::jpeg_common_struct) {
    let code = unsafe { cinfo.err.as_ref().map_or(0, |e| e.msg_code) };
    std::panic::resume_unwind(Box::new(format!("libjpeg error {code}")));
}

extern "C-unwind" fn silence(_: &mut ffi::jpeg_common_struct, _: c_int) {}

/// Frees libjpeg state even when an error unwinds out of the transcode.
struct Guard {
    src: Box<ffi::jpeg_decompress_struct>,
    dst: Box<ffi::jpeg_compress_struct>,
    out: *mut u8,
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            ffi::jpeg_destroy_compress(&mut self.dst);
            ffi::jpeg_destroy_decompress(&mut self.src);
            if !self.out.is_null() {
                libc::free(self.out.cast());
            }
        }
    }
}

unsafe fn transcode(data: &[u8], orientation: u8) -> Vec<u8> {
    unsafe {
        let mut err: Box<ffi::jpeg_error_mgr> = Box::new(mem::zeroed());
        ffi::jpeg_std_error(&mut err);
        err.error_exit = Some(error_exit);
        err.emit_message = Some(silence);

        let mut g = Guard { src: Box::new(mem::zeroed()), dst: Box::new(mem::zeroed()), out: ptr::null_mut() };
        g.src.common.err = &mut *err;
        ffi::jpeg_create_decompress(&mut *g.src);
        g.dst.common.err = &mut *err;
        ffi::jpeg_create_compress(&mut *g.dst);

        ffi::jpeg_mem_src(&mut g.src, data.as_ptr(), data.len() as c_ulong);
        ffi::jpeg_save_markers(&mut g.src, APP2, 0xFFFF);
        ffi::jpeg_read_header(&mut g.src, 1);
        let coefficients = ffi::jpeg_read_coefficients(&mut g.src);

        ffi::jpeg_copy_critical_parameters(&g.src, &mut g.dst);
        g.dst.optimize_coding = 1;
        ffi::jpeg_simple_progression(&mut g.dst);

        let mut size: c_ulong = 0;
        ffi::jpeg_mem_dest(&mut g.dst, &mut g.out, &mut size);
        ffi::jpeg_write_coefficients(&mut g.dst, coefficients);

        if orientation > 1 {
            let exif = orientation_exif(orientation);
            ffi::jpeg_write_marker(&mut g.dst, APP1, exif.as_ptr(), exif.len() as c_uint);
        }
        let mut marker = g.src.marker_list;
        while let Some(m) = marker.as_ref() {
            if c_int::from(m.marker) == APP2 {
                ffi::jpeg_write_marker(&mut g.dst, APP2, m.data, m.data_length);
            }
            marker = m.next;
        }

        ffi::jpeg_finish_compress(&mut g.dst);
        ffi::jpeg_finish_decompress(&mut g.src);
        std::slice::from_raw_parts(g.out, size as usize).to_vec()
    }
}

/// Smallest valid EXIF block: a single Orientation tag.
fn orientation_exif(orientation: u8) -> Vec<u8> {
    let mut v = b"Exif\0\0".to_vec();
    v.extend_from_slice(b"MM\0\x2A\0\0\0\x08"); // big-endian TIFF header, IFD at offset 8
    v.extend_from_slice(&1u16.to_be_bytes()); // one entry
    v.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    v.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    v.extend_from_slice(&1u32.to_be_bytes()); // count
    v.extend_from_slice(&u16::from(orientation).to_be_bytes());
    v.extend_from_slice(&[0, 0]); // value padding
    v.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageDecoder;

    fn decode(data: &[u8]) -> (image::RgbImage, u8) {
        let mut dec = image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Jpeg)
            .into_decoder()
            .unwrap();
        let orientation = dec.orientation().unwrap().to_exif();
        (image::DynamicImage::from_decoder(dec).unwrap().to_rgb8(), orientation)
    }

    #[test]
    fn pixels_are_identical_and_orientation_is_kept() {
        let src = image::RgbImage::from_fn(320, 200, |x, y| {
            image::Rgb([(x * 7 % 256) as u8, (y * 3 % 256) as u8, ((x ^ y) % 256) as u8])
        });
        let settings = crate::EncodeSettings {
            format: crate::OutputFormat::Jpeg,
            quality: 85,
            lossless: false,
            effort: crate::Effort::Fast, // baseline, not optimized: room for the optimizer
        };
        let original = crate::encode(&image::DynamicImage::ImageRgb8(src), &settings).unwrap();

        let optimized = optimize(&original, 6).unwrap();
        assert!(optimized.len() < original.len(), "{} >= {}", optimized.len(), original.len());
        let (a, _) = decode(&original);
        let (b, orientation) = decode(&optimized);
        assert_eq!(a.as_raw(), b.as_raw(), "pixels changed");
        assert_eq!(orientation, 6);
    }

    #[test]
    fn rejects_non_jpeg() {
        assert!(optimize(b"not a jpeg at all", 1).is_err());
    }
}
