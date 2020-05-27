use super::texture::{AtlasAllocation, TextureAtlas};
use image::Pixel;
use pathfinder_geometry::{
    transform2d::Transform2F,
    vector::{Vector2F, Vector2I},
};

pub struct PreRenderedGlyph {
    pub glyph_allocation: Option<AtlasAllocation>,
    pub advance: f32,
    pub x: f32,
    pub y: f32,
}

pub struct GLFont {
    font: font_kit::font::Font,
    glyphs: std::collections::hash_map::HashMap<u32, PreRenderedGlyph>,
    pub pixel_size: f32,
    pub metrics: font_kit::metrics::Metrics,
}

impl GLFont {
    pub fn new(font: font_kit::font::Font, pixel_size: f32) -> Self {
        let glyphs = std::collections::hash_map::HashMap::new();
        let metrics = font.metrics();
        Self { font, glyphs, pixel_size, metrics }
    }

    pub fn string_to_glyphs(
        &mut self,
        gl: &glow::Context,
        atlas: &mut TextureAtlas,
        text: &str,
    ) -> Vec<u32> {
        text.chars()
            .map(|ch| {
                let glyph = self.font.glyph_for_char(ch).unwrap();

                if !self.glyphs.contains_key(&glyph) {
                    // ensure the glyph is cached
                    self.glyphs.insert(glyph, self.render_glyph(gl, atlas, glyph));
                }

                glyph
            })
            .collect()
    }

    pub fn layout_glyphs<'a, I: std::iter::IntoIterator<Item = u32>>(
        &'a mut self,
        glyphs: I,
    ) -> GlyphIter<'a, I::IntoIter> {
        GlyphIter { gl_font: self, glyph_it: glyphs.into_iter() }
    }

    fn render_glyph(
        &self,
        gl: &glow::Context,
        atlas: &mut TextureAtlas,
        glyph_id: u32,
    ) -> PreRenderedGlyph {
        let scale_from_font_units = self.pixel_size / self.metrics.units_per_em as f32;

        let advance = self.font.advance(glyph_id).unwrap().x() * scale_from_font_units;

        let hinting = font_kit::hinting::HintingOptions::None;
        let raster_opts = font_kit::canvas::RasterizationOptions::GrayscaleAa;

        let glyph_rect = self.font.typographic_bounds(glyph_id).unwrap();

        let glyph_width = glyph_rect.width() * scale_from_font_units;
        let glyph_height = glyph_rect.height() * scale_from_font_units;
        let x = glyph_rect.origin_x() * scale_from_font_units;
        let y = -(glyph_rect.origin_y() + glyph_rect.height()) * scale_from_font_units;

        let glyph_allocation = if glyph_width > 0. && glyph_height > 0. {
            let mut canvas = font_kit::canvas::Canvas::new(
                Vector2I::new(glyph_width.ceil() as i32, glyph_height.ceil() as i32),
                font_kit::canvas::Format::A8,
            );
            self.font
                .rasterize_glyph(
                    &mut canvas,
                    glyph_id,
                    self.pixel_size,
                    Transform2F::from_translation(Vector2F::new(-x, -y.ceil() - 5.)),
                    hinting,
                    raster_opts,
                )
                .unwrap();

            let mut glyph_image = image::ImageBuffer::from_pixel(
                canvas.size.x() as u32,
                canvas.size.y() as u32,
                image::Rgba::<u8>::from_channels(0, 255, 0, 0),
            );
            for (x, y, pixel) in glyph_image.enumerate_pixels_mut() {
                let idx = (x as usize) + (y as usize) * canvas.stride;
                let alpha = canvas.pixels[idx];
                *pixel = image::Rgba::<u8>::from_channels(0, 0, 0, alpha)
            }
            /*
            let glyph_image = image::ImageBuffer::from_fn(
                canvas.size.x() as u32,
                canvas.size.y() as u32,
                |x, y| {
                    let idx = (x as usize) + (y as usize) * canvas.stride;
                    let alpha = canvas.pixels[idx];
                    image::Rgba::<u8>::from_channels(0, 0, 0, alpha)
                },
            );
            */

            Some(atlas.allocate_image_in_atlas(gl, glyph_image))
        } else {
            None
        };

        PreRenderedGlyph { glyph_allocation, advance, x, y }
    }
}

pub struct GlyphIter<'a, GlyphIterator> {
    gl_font: &'a GLFont,
    glyph_it: GlyphIterator,
}

impl<'a, GlyphIterator> Iterator for GlyphIter<'a, GlyphIterator>
where
    GlyphIterator: std::iter::Iterator<Item = u32>,
{
    type Item = &'a PreRenderedGlyph;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(glyph_id) = self.glyph_it.next() {
            Some(&self.gl_font.glyphs[&glyph_id])
        } else {
            None
        }
    }
}
