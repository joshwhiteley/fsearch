//! fsearch's own libchafa-backed cell-art renderer, used for image previews
//! in terminals without a graphics protocol. Differs from ratatui-image's
//! chafa fallback in two ways that matter for legibility: the symbol map is
//! restricted to geometric shading glyphs (no ASCII/letters speckling the
//! picture), and the work factor is set to maximum quality.

#[cfg(feature = "chafa")]
use ratatui::style::{Color, Style};
#[cfg(feature = "chafa")]
use ratatui::text::{Line, Span};

/// Aspect-correct cell geometry for an image in a `cols` x `rows` area,
/// assuming terminal cells are twice as tall as wide. Fills as much of the
/// area as possible.
pub fn fit_cells(img_w: u32, img_h: u32, cols: u16, rows: u16) -> (u16, u16) {
    if img_w == 0 || img_h == 0 || cols == 0 || rows == 0 {
        return (1, 1);
    }
    let aspect = img_w as f64 / img_h as f64;
    // one row is worth two columns of pixels
    let y = f64::from(rows).min(f64::from(cols) / (2.0 * aspect));
    let x = 2.0 * aspect * y;
    (
        (x.round().max(1.0) as u16).min(cols),
        (y.round().max(1.0) as u16).min(rows),
    )
}

#[cfg(feature = "chafa")]
mod chafa_ffi {
    use std::ffi::c_void;

    pub type Canvas = *mut c_void;
    pub type CanvasConfig = *mut c_void;
    pub type SymbolMap = *mut c_void;

    // geometric shading glyphs only — no ASCII/letters/braille speckle
    pub const TAGS_GEOMETRIC: i32 = (1 << 0)   // space
        | (1 << 1)                             // solid
        | (1 << 3)                             // block
        | (1 << 7)                             // quad
        | (1 << 8)                             // hhalf
        | (1 << 9)                             // vhalf
        | (1 << 22)                            // sextant
        | (1 << 23)                            // wedge
        | (1 << 26); // octant
    pub const PIXEL_RGB8: i32 = 8;

    #[link(name = "chafa")]
    unsafe extern "C" {
        pub fn chafa_symbol_map_new() -> SymbolMap;
        pub fn chafa_symbol_map_add_by_tags(map: SymbolMap, tags: i32);
        pub fn chafa_canvas_config_new() -> CanvasConfig;
        pub fn chafa_canvas_config_set_symbol_map(config: CanvasConfig, map: SymbolMap);
        pub fn chafa_canvas_config_set_geometry(config: CanvasConfig, width: i32, height: i32);
        pub fn chafa_canvas_config_set_work_factor(config: CanvasConfig, work_factor: f32);
        pub fn chafa_canvas_config_unref(config: CanvasConfig);
        pub fn chafa_canvas_new(config: CanvasConfig) -> Canvas;
        pub fn chafa_canvas_draw_all_pixels(
            canvas: Canvas,
            pixel_type: i32,
            pixels: *const u8,
            width: i32,
            height: i32,
            rowstride: i32,
        );
        pub fn chafa_canvas_get_char_at(canvas: Canvas, x: i32, y: i32) -> u32;
        pub fn chafa_canvas_get_colors_at(
            canvas: Canvas,
            x: i32,
            y: i32,
            fg: *mut i32,
            bg: *mut i32,
        );
        pub fn chafa_canvas_unref(canvas: Canvas);
    }
}

/// Renders `img` as styled text lines sized `cols` x `rows` (already
/// aspect-fitted by the caller via [`fit_cells`]).
#[cfg(feature = "chafa")]
pub fn render(img: &image::DynamicImage, cols: u16, rows: u16) -> Vec<Line<'static>> {
    use chafa_ffi::*;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut lines = Vec::with_capacity(rows as usize);
    unsafe {
        let map = chafa_symbol_map_new();
        chafa_symbol_map_add_by_tags(map, TAGS_GEOMETRIC);
        let config = chafa_canvas_config_new();
        chafa_canvas_config_set_symbol_map(config, map);
        chafa_canvas_config_set_geometry(config, i32::from(cols), i32::from(rows));
        chafa_canvas_config_set_work_factor(config, 1.0);
        let canvas = chafa_canvas_new(config);
        chafa_canvas_draw_all_pixels(
            canvas,
            PIXEL_RGB8,
            rgb.as_ptr(),
            w as i32,
            h as i32,
            (w * 3) as i32,
        );
        for y in 0..rows {
            let mut spans = Vec::with_capacity(cols as usize);
            for x in 0..cols {
                let c = chafa_canvas_get_char_at(canvas, i32::from(x), i32::from(y));
                let symbol = char::from_u32(c).filter(|c| !c.is_control()).unwrap_or(' ');
                let (mut fg, mut bg) = (0i32, 0i32);
                chafa_canvas_get_colors_at(canvas, i32::from(x), i32::from(y), &mut fg, &mut bg);
                let style = Style::default()
                    .fg(Color::Rgb(
                        ((fg >> 16) & 0xff) as u8,
                        ((fg >> 8) & 0xff) as u8,
                        (fg & 0xff) as u8,
                    ))
                    .bg(Color::Rgb(
                        ((bg >> 16) & 0xff) as u8,
                        ((bg >> 8) & 0xff) as u8,
                        (bg & 0xff) as u8,
                    ));
                spans.push(Span::styled(symbol.to_string(), style));
            }
            lines.push(Line::from(spans));
        }
        chafa_canvas_unref(canvas);
        chafa_canvas_config_unref(config);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_preserves_aspect_within_area() {
        // square image in a wide area: height-bound
        assert_eq!(fit_cells(100, 100, 120, 20), (40, 20));
        // wide image in a narrow area: width-bound
        assert_eq!(fit_cells(400, 100, 40, 40), (40, 5));
        // degenerate inputs stay sane
        assert_eq!(fit_cells(0, 100, 40, 40), (1, 1));
    }

    #[cfg(feature = "chafa")]
    #[test]
    fn renders_solid_color_cells() {
        use ratatui::style::Color;
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            64,
            64,
            image::Rgb([200, 30, 30]),
        ));
        let lines = render(&img, 10, 5);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].spans.len(), 10);
        // a solid red image must produce red-dominant cell colors
        let style = lines[2].spans[5].style;
        let red_ish = |c: Option<Color>| matches!(c, Some(Color::Rgb(r, g, b)) if r > 150 && g < 90 && b < 90);
        assert!(red_ish(style.fg) || red_ish(style.bg));
    }
}
