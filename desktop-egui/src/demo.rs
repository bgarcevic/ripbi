//! Records the README's demo GIF: a scripted session rendered headlessly by
//! egui_kittest, so it is reproducible and never touches the real screen.
//!
//! ```sh
//! cargo test --release --manifest-path desktop-egui/Cargo.toml demo -- --ignored
//! ```
//!
//! It writes `docs/media/ripbi-desktop.gif` from `samples/AdventureWorks Sales.pbip`.
//! The model's full path shows in the recording; on Windows, map the checkout
//! to a drive first (`subst R: .`, then `RIPBI_DEMO_ROOT=R:\`) to keep it short.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::accesskit::Role;
use eframe::egui::{self, Event, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_system_font_provider::SystemFontProvider;
use image::RgbaImage;

use super::{App, update};

const SIZE: egui::Vec2 = vec2(1200.0, 740.0);

/// One recorded frame and how long it shows, in hundredths of a second.
struct Frame {
    image: RgbaImage,
    delay: u16,
}

struct Demo<'a> {
    harness: Harness<'a, App>,
    frames: Vec<Frame>,
    cursor: Pos2,
}

impl Demo<'_> {
    fn new() -> Self {
        let harness = Harness::builder()
            .with_size(SIZE)
            .with_pixels_per_point(1.0)
            .with_render_cursor(false)
            // A recording, not an accessibility test.
            .with_accessibility_check(false)
            .wgpu()
            .with_context_setup(|ctx| {
                // As eframe does for the real window: emoji and symbols.
                ctx.add_font_provider(Arc::new(SystemFontProvider::new()));
                super::styled(ctx);
                ctx.set_theme(egui::ThemePreference::Dark);
            })
            .build_eframe(|_| App::default());
        Self {
            harness,
            frames: Vec::new(),
            cursor: pos2(SIZE.x * 0.62, SIZE.y * 0.7),
        }
    }

    /// Runs one frame and records it with the cursor painted on top.
    fn shoot(&mut self, delay: u16, ripple: Option<f32>) {
        self.harness.step();
        let mut image = self.harness.render().expect("render a frame");
        if let Some(progress) = ripple {
            paint_ripple(&mut image, self.cursor, progress);
        }
        paint_cursor(&mut image, self.cursor);
        self.frames.push(Frame { image, delay });
    }

    fn hold(&mut self, seconds: f32) {
        self.harness.run_ok();
        self.shoot((seconds * 100.0) as u16, None);
    }

    /// Glides the cursor to `target` with an ease-in-out curve.
    fn glide(&mut self, target: Pos2) {
        let start = self.cursor;
        let steps = ((target - start).length() / 45.0).clamp(8.0, 18.0) as usize;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let eased = t * t * (3.0 - 2.0 * t);
            self.cursor = start + (target - start) * eased;
            self.harness.event(Event::PointerMoved(self.cursor));
            self.shoot(3, None);
        }
    }

    /// A press-and-release at the cursor, with a ripple. `real` sends the
    /// click to the app; a fake one only shows it (for the file dialog).
    fn click(&mut self, real: bool) {
        if real {
            for pressed in [true, false] {
                self.harness.event(Event::PointerButton {
                    pos: self.cursor,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        for progress in [0.25, 0.5, 0.75, 1.0] {
            self.shoot(4, Some(progress));
        }
    }

    fn click_label(&mut self, label: &str) {
        let target = self.harness.get_by_label_contains(label).rect().center();
        self.glide(target);
        self.click(true);
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.harness.event(Event::Text(ch.to_string()));
            self.shoot(9, None);
        }
    }

    /// Records the spinner until the analysis worker reports back.
    fn wait_for_analysis(&mut self) {
        let started = Instant::now();
        while self.harness.state().job.is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "analysis timed out"
            );
            std::thread::sleep(Duration::from_millis(80));
            self.shoot(8, None);
        }
    }

    fn save(self, path: &Path) {
        encode_gif(&self.frames, path);
    }
}

#[test]
#[ignore = "renders the README demo GIF; run explicitly"]
fn demo() {
    // The recording shows the model's full path: RIPBI_DEMO_ROOT names a
    // neutral checkout (e.g. `subst R: .` on Windows) instead of this one.
    let root = std::env::var_os("RIPBI_DEMO_ROOT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
        PathBuf::from,
    );
    let mut demo = Demo::new();
    // System (dark here), so one click on the toggle shows the light theme.
    demo.harness.ctx.set_theme(egui::ThemePreference::System);
    demo.hold(1.6);

    // Pick the model. The real button opens a file dialog, so only the click
    // is shown and the model is set as the dialog would.
    let choose = demo.harness.get_by_label("Choose .pbip…").rect().center();
    demo.glide(choose);
    demo.click(false);
    demo.harness
        .state_mut()
        .set_model(root.join("samples").join("AdventureWorks Sales.pbip"));
    demo.hold(0.8);

    demo.click_label("Analyze model");
    demo.wait_for_analysis();
    demo.hold(2.4);

    // Narrow to measures, then search.
    // The type list has no accessible name; it sits right of the search box.
    let search = demo.harness.get_by_role(Role::TextInput).rect();
    let kinds = egui::Rect::from_min_size(
        pos2(search.right() + 8.0, search.top()),
        vec2(120.0, search.height()),
    );
    demo.glide(kinds.center());
    demo.click(true);
    demo.hold(0.5);
    let measure = demo
        .harness
        .query_all_by_label("measure")
        .map(|node| node.rect())
        .filter(|rect| rect.top() > kinds.bottom())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("the type list offers measures");
    demo.glide(measure.center());
    demo.click(true);
    demo.hold(0.6);

    demo.glide(search.center());
    demo.click(true);
    demo.type_text("sales");
    demo.hold(1.0);

    // Select a row beside its text: labels take their own clicks.
    let finding = {
        let analysis = demo.harness.state().analysis.as_ref().expect("analysis");
        analysis
            .output
            .findings
            .iter()
            .find(|f| f.kind == "measure" && f.id.to_lowercase().contains("sales"))
            .expect("the sample has an unused sales measure")
            .id
            .clone()
    };
    let row = demo.harness.get_by_label(&finding).rect();
    demo.glide(pos2(row.right() + 24.0, row.center().y));
    demo.click(true);
    demo.hold(2.6);

    // Switch to the light theme with the header's toggle.
    demo.glide(pos2(SIZE.x - 17.0, 17.0));
    demo.click(true);
    demo.hold(1.8);

    // A newer release has been downloaded and verified.
    demo.harness.state_mut().staged = Some(update::Staged::preview());
    demo.harness.run_ok();
    let restart = demo
        .harness
        .get_by_label("Restart to update")
        .rect()
        .center();
    demo.glide(restart);
    demo.hold(2.6);

    let out = std::env::var_os("RIPBI_DEMO_OUT")
        .map_or_else(|| root.join("docs/media/ripbi-desktop.gif"), PathBuf::from);
    demo.save(&out);
}

/// The classic arrow pointer, tip at the origin.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.2, 13.2),
    (7.0, 19.5),
    (9.8, 18.3),
    (7.1, 12.2),
    (12.4, 12.2),
];

fn inside(x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut previous = ARROW[ARROW.len() - 1];
    for &(xi, yi) in &ARROW {
        let (xj, yj) = previous;
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        previous = (xi, yi);
    }
    inside
}

fn paint_cursor(image: &mut RgbaImage, at: Pos2) {
    for dy in -1..22 {
        for dx in -1..15 {
            let (x, y) = (at.x as i32 + dx, at.y as i32 + dy);
            if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
                continue;
            }
            let (cx, cy) = (dx as f32 + 0.5, dy as f32 + 0.5);
            if !inside(cx, cy) {
                continue;
            }
            let edge = [(1.2, 0.0), (-1.2, 0.0), (0.0, 1.2), (0.0, -1.2)]
                .iter()
                .any(|(ox, oy)| !inside(cx + ox, cy + oy));
            let value = if edge { 0 } else { 255 };
            image.put_pixel(x as u32, y as u32, image::Rgba([value, value, value, 255]));
        }
    }
}

/// A ring growing out of the cursor tip and fading as it goes.
fn paint_ripple(image: &mut RgbaImage, at: Pos2, progress: f32) {
    let radius = 6.0 + 16.0 * progress;
    let alpha = 0.75 * (1.0 - progress) + 0.15;
    let color = [0x2e_u8, 0xa0, 0x43];
    let reach = radius as i32 + 3;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let distance = ((dx * dx + dy * dy) as f32).sqrt();
            let coverage = (2.0 - (distance - radius).abs()).clamp(0.0, 1.0) * alpha;
            let (x, y) = (at.x as i32 + dx, at.y as i32 + dy);
            if coverage <= 0.0
                || x < 0
                || y < 0
                || x >= image.width() as i32
                || y >= image.height() as i32
            {
                continue;
            }
            let pixel = image.get_pixel_mut(x as u32, y as u32);
            for channel in 0..3 {
                let base = f32::from(pixel[channel]);
                pixel[channel] = (base + (f32::from(color[channel]) - base) * coverage) as u8;
            }
        }
    }
}

/// Writes the frames as a looping GIF. One palette serves every frame, and
/// each frame stores only the rectangle that changed, which keeps a UI
/// recording small.
fn encode_gif(frames: &[Frame], path: &Path) {
    let (width, height) = frames[0].image.dimensions();
    let mut sample = Vec::new();
    for frame in frames.iter().step_by(4) {
        sample.extend(
            frame
                .image
                .as_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .step_by(13)
                .flatten(),
        );
    }
    let quant = color_quant::NeuQuant::new(10, 256, &sample);
    let palette = quant.color_map_rgb();
    let mut cache: HashMap<[u8; 4], u8> = HashMap::new();
    let mut index = |pixel: &[u8]| {
        let key = [pixel[0], pixel[1], pixel[2], 255];
        *cache
            .entry(key)
            .or_insert_with(|| quant.index_of(&key) as u8)
    };

    let file = File::create(path).expect("create the GIF");
    let mut encoder =
        gif::Encoder::new(file, width as u16, height as u16, &palette).expect("start the GIF");
    encoder
        .set_repeat(gif::Repeat::Infinite)
        .expect("loop the GIF");

    let mut previous: Option<&RgbaImage> = None;
    let mut pending: Option<gif::Frame<'static>> = None;
    for frame in frames {
        let image = &frame.image;
        let changed = previous.map_or(Some((0, 0, width - 1, height - 1)), |before| {
            changed_rect(before, image)
        });
        match changed {
            None => {
                // Nothing moved: show the previous frame longer.
                if let Some(last) = &mut pending {
                    last.delay = last.delay.saturating_add(frame.delay);
                }
            }
            Some((left, top, right, bottom)) => {
                if let Some(last) = pending.take() {
                    encoder.write_frame(&last).expect("write a GIF frame");
                }
                let mut buffer =
                    Vec::with_capacity(((right - left + 1) * (bottom - top + 1)) as usize);
                for y in top..=bottom {
                    for x in left..=right {
                        buffer.push(index(&image.get_pixel(x, y).0));
                    }
                }
                pending = Some(gif::Frame {
                    left: left as u16,
                    top: top as u16,
                    width: (right - left + 1) as u16,
                    height: (bottom - top + 1) as u16,
                    delay: frame.delay,
                    dispose: gif::DisposalMethod::Keep,
                    buffer: Cow::Owned(buffer),
                    ..gif::Frame::default()
                });
                previous = Some(image);
            }
        }
    }
    if let Some(last) = pending {
        encoder.write_frame(&last).expect("write a GIF frame");
    }
}

/// The bounding box of the pixels that differ, inclusive.
fn changed_rect(before: &RgbaImage, after: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = after.dimensions();
    let mut rect: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                rect = Some(rect.map_or((x, y, x, y), |(l, t, r, b)| {
                    (l.min(x), t.min(y), r.max(x), b.max(y))
                }));
            }
        }
    }
    rect
}
