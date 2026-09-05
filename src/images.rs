use image::{DynamicImage, ImageDecoder};

/// Image preview skips files larger than this (decoding is done in memory).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 32_000_000;
const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;

const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "ico", "qoi", "svg",
];

pub fn is_image_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Decodes an image file (rasterizing SVGs) into a `DynamicImage`.
pub fn load(path: &str, max_bytes: u64) -> Result<DynamicImage, String> {
    let file =
        crate::util::open_regular_file(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular image file".into());
    }
    if meta.len() > max_bytes {
        return Err(format!(
            "image larger than {} MiB",
            max_bytes / (1024 * 1024)
        ));
    }
    let is_svg = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        use std::io::Read;
        let mut data = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() as u64 > max_bytes {
            return Err("SVG grew beyond the input-size limit".into());
        }
        rasterize_svg(&data)
    } else {
        let format = image::ImageFormat::from_path(path).map_err(|e| e.to_string())?;
        let mut reader = image::ImageReader::with_format(std::io::BufReader::new(file), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        reader.limits(limits.clone());
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (width, height) = decoder.dimensions();
        if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err("image exceeds the 32 megapixel preview limit".into());
        }
        limits
            .reserve(decoder.total_bytes())
            .map_err(|e| e.to_string())?;
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())
    }
}

/// Vector art is resolution-free: always rasterize with the long edge at
/// this many pixels so previews stay crisp at any zoom.
const SVG_EDGE: f32 = 1600.0;

fn rasterize_svg(data: &[u8]) -> Result<DynamicImage, String> {
    let tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = SVG_EDGE / size.width().max(size.height());
    let (w, h) = (
        (size.width() * scale).round().max(1.0) as u32,
        (size.height() * scale).round().max(1.0) as u32,
    );
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(w, h).ok_or_else(|| "svg has zero size".to_string())?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let img = image::RgbaImage::from_raw(w, h, pixmap.take())
        .ok_or_else(|| "svg raster buffer mismatch".to_string())?;
    Ok(DynamicImage::ImageRgba8(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_image_extensions() {
        for p in [
            "/a/photo.png",
            "/a/photo.JPG",
            "/a/scan.tiff",
            "/a/scan.tif",
            "/a/anim.gif",
            "/a/pic.webp",
            "/a/pic.bmp",
            "/a/icon.svg",
        ] {
            assert!(is_image_path(p), "should be image: {p}");
        }
        for p in ["/a/notes.md", "/a/code.rs", "/a/archive.tar.gz", "/a/png"] {
            assert!(!is_image_path(p), "should not be image: {p}");
        }
    }

    #[test]
    fn loads_a_png_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dot.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let img = load(path.to_str().unwrap(), MAX_IMAGE_BYTES).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
    }

    #[test]
    fn rasterizes_svg_scaled_up_for_crisp_previews() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("box.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#00ff00"/></svg>"##,
        )
        .unwrap();
        let img = load(path.to_str().unwrap(), MAX_IMAGE_BYTES).unwrap();
        // long edge lands on SVG_EDGE, aspect preserved
        assert_eq!((img.width(), img.height()), (1600, 800));
        // the rect actually rendered
        let px = img.to_rgba8().get_pixel(100, 100).0;
        assert_eq!(px, [0, 255, 0, 255]);
    }

    #[test]
    fn oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]))
            .save(&path)
            .unwrap();
        assert!(load(path.to_str().unwrap(), 10).is_err());
    }

    #[test]
    fn corrupt_image_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.png");
        std::fs::write(&path, b"not a png").unwrap();
        assert!(load(path.to_str().unwrap(), MAX_IMAGE_BYTES).is_err());
    }

    #[test]
    fn extreme_dimensions_are_refused_before_full_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        image::RgbaImage::new(16_385, 1).save(&path).unwrap();
        assert!(load(path.to_str().unwrap(), MAX_IMAGE_BYTES).is_err());
    }
}
