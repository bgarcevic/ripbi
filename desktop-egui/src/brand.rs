//! The ripbi mark: a dependency tree on a green tile. The root reaches one
//! live node; its other branch is ripped and that node hollow, the unused
//! object ripbi finds. Defined once on a 64-unit grid and drawn two ways:
//! painted in the header and rasterized for the window icon.
//! `docs/media/ripbi-mark.svg` is the same geometry.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2, pos2};

const TILE: Color32 = Color32::from_rgb(0x2e, 0xa0, 0x43);
const GRID: f32 = 64.0;
const CORNER: f32 = 14.0;
const NODE: f32 = 6.0;
const EDGE: f32 = 4.0;
const RING: f32 = 3.0;
/// Opacity of the ripped branch.
const FADED: f32 = 0.45;

const ROOT: Pos2 = pos2(32.0, 17.0);
const LIVE: Pos2 = pos2(19.0, 46.0);
const DEAD: Pos2 = pos2(45.0, 46.0);
/// The ripped edge's two pieces, as fractions of root → dead node.
const PIECES: [(f32, f32); 2] = [(0.0, 0.34), (0.56, 0.68)];

enum Mark {
    Disc(Pos2, f32),
    Ring(Pos2, f32, f32),
    Bar(Pos2, Pos2, f32),
}

/// White shapes over the tile, back to front, with their opacity. None
/// overlap at partial opacity, so drawing order only matters at the root.
fn marks() -> [(Mark, f32); 6] {
    let along = |t: f32| ROOT + (DEAD - ROOT) * t;
    [
        (Mark::Bar(ROOT, LIVE, EDGE), 1.0),
        (
            Mark::Bar(along(PIECES[0].0), along(PIECES[0].1), EDGE),
            FADED,
        ),
        (
            Mark::Bar(along(PIECES[1].0), along(PIECES[1].1), EDGE),
            FADED,
        ),
        (Mark::Ring(DEAD, NODE, RING), FADED),
        (Mark::Disc(ROOT, NODE), 1.0),
        (Mark::Disc(LIVE, NODE), 1.0),
    ]
}

/// Paints the mark filling `rect` (keep it square).
pub fn paint(painter: &egui::Painter, rect: Rect) {
    let scale = rect.width() / GRID;
    let at = |p: Pos2| rect.min + p.to_vec2() * scale;
    painter.rect_filled(rect, CORNER * scale, TILE);
    for (mark, opacity) in marks() {
        let white = Color32::WHITE.gamma_multiply(opacity);
        match mark {
            Mark::Disc(c, r) => {
                painter.circle_filled(at(c), r * scale, white);
            }
            Mark::Ring(c, r, w) => {
                painter.circle_stroke(at(c), r * scale, Stroke::new(w * scale, white));
            }
            Mark::Bar(a, b, w) => {
                painter.line_segment([at(a), at(b)], Stroke::new(w * scale, white));
            }
        }
    }
}

/// The mark as a `size`×`size` window icon, antialiased from signed
/// distances so no image file or decoder is needed.
pub fn icon(size: u32) -> egui::IconData {
    let pixel = GRID / size as f32;
    let cover = |distance: f32| (0.5 - distance / pixel).clamp(0.0, 1.0);
    let marks = marks();
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = pos2((x as f32 + 0.5) * pixel, (y as f32 + 0.5) * pixel);
            let alpha = cover(rounded_square(p));
            let mut rgb = [TILE.r(), TILE.g(), TILE.b()].map(f32::from);
            for (mark, opacity) in &marks {
                let a = cover(distance(mark, p)) * opacity;
                rgb = rgb.map(|c| c + (255.0 - c) * a);
            }
            rgba.extend(rgb.map(|c| c.round() as u8));
            rgba.push((alpha * 255.0).round() as u8);
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

fn rounded_square(p: Pos2) -> f32 {
    let half = GRID / 2.0;
    let q = (p - pos2(half, half)).abs() - Vec2::splat(half - CORNER);
    q.max(Vec2::ZERO).length() + q.x.max(q.y).min(0.0) - CORNER
}

fn distance(mark: &Mark, p: Pos2) -> f32 {
    match *mark {
        Mark::Disc(c, r) => p.distance(c) - r,
        Mark::Ring(c, r, w) => (p.distance(c) - r).abs() - w / 2.0,
        Mark::Bar(a, b, w) => {
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0);
            p.distance(a + ab * t) - w / 2.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The icon files each platform reads from disk, generated from the
    /// mark: the exe's embedded `.ico` (Windows), the bundle's `.icns`
    /// (macOS) and the `.desktop` file's PNG (Linux). Fails when they drift
    /// from the geometry; `RIPBI_WRITE_ICONS=1 cargo test` rewrites them.
    #[test]
    fn icon_files_match_the_mark() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let files = [
            ("ripbi.ico", ico(&[16, 24, 32, 48, 64, 256])),
            (
                "ripbi.icns",
                icns(&[
                    (*b"icp4", 16),
                    (*b"icp5", 32),
                    (*b"ic07", 128),
                    (*b"ic08", 256),
                    (*b"ic09", 512),
                    (*b"ic10", 1024),
                ]),
            ),
            ("ripbi.png", png(256)),
        ];
        let write = std::env::var_os("RIPBI_WRITE_ICONS").is_some();
        for (name, bytes) in files {
            let path = dir.join(name);
            if write {
                std::fs::write(&path, &bytes).unwrap();
            } else {
                assert!(
                    std::fs::read(&path).ok() == Some(bytes),
                    "assets/{name} is stale: run `RIPBI_WRITE_ICONS=1 cargo test`"
                );
            }
        }
    }

    fn png(size: u32) -> Vec<u8> {
        let icon = icon(size);
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&icon.rgba).unwrap();
        writer.finish().unwrap();
        out
    }

    /// An ICO of PNG images, which Windows reads since Vista.
    fn ico(sizes: &[u32]) -> Vec<u8> {
        let images: Vec<_> = sizes.iter().map(|&size| png(size)).collect();
        let mut out = Vec::new();
        out.extend(0u16.to_le_bytes());
        out.extend(1u16.to_le_bytes()); // icon
        out.extend((sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (&size, image) in sizes.iter().zip(&images) {
            let side = if size >= 256 { 0 } else { size as u8 }; // 0 means 256
            out.extend([side, side, 0, 0]);
            out.extend(1u16.to_le_bytes()); // planes
            out.extend(32u16.to_le_bytes()); // bits per pixel
            out.extend((image.len() as u32).to_le_bytes());
            out.extend(offset.to_le_bytes());
            offset += image.len() as u32;
        }
        images.iter().for_each(|image| out.extend(image));
        out
    }

    /// An ICNS of PNG images, each under its size's type code.
    fn icns(entries: &[([u8; 4], u32)]) -> Vec<u8> {
        let mut body = Vec::new();
        for &(kind, size) in entries {
            let image = png(size);
            body.extend(kind);
            body.extend((image.len() as u32 + 8).to_be_bytes());
            body.extend(image);
        }
        let mut out = b"icns".to_vec();
        out.extend((body.len() as u32 + 8).to_be_bytes());
        out.extend(body);
        out
    }

    fn pixel(icon: &egui::IconData, at: Pos2) -> [u8; 4] {
        let scale = icon.width as f32 / GRID;
        let (x, y) = ((at.x * scale) as usize, (at.y * scale) as usize);
        let i = (y * icon.width as usize + x) * 4;
        icon.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn icon_is_a_green_tile_with_a_white_live_node() {
        let icon = icon(64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!(
            pixel(&icon, pos2(0.5, 0.5))[3],
            0,
            "corners are transparent"
        );
        assert_eq!(pixel(&icon, pos2(32.0, 60.0)), [0x2e, 0xa0, 0x43, 255]);
        assert_eq!(pixel(&icon, LIVE), [255, 255, 255, 255]);
        let hollow = pixel(&icon, DEAD);
        assert_eq!(hollow, [0x2e, 0xa0, 0x43, 255], "the dead node is hollow");
    }
}
