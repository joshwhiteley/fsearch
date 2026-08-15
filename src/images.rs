use image::DynamicImage;

/// Image preview skips files larger than this (decoding is done in memory).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

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
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
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
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        rasterize_svg(&data)
    } else {
        image::open(path).map_err(|e| e.to_string())
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
}
