//! Images in the Markdown viewer. An image that is a paragraph of its own
//! (`![alt](path)` on its own lines) and names a local file is drawn in
//! place: the renderer reserves rows for it (`ImageSpot`) and the viewer
//! paints the picture over them (`crate::ui::markdown`), with its
//! `[image: alt]` caption below. Inline images, remote ones and files that
//! are not images stay a caption alone. An SVG is rasterized (`resvg`, with
//! the system's fonts, over white) at the size it is drawn.
//!
//! The picture goes through the terminal's graphics protocol when agent-mux
//! can tell which one it speaks, from the environment: kitty and Ghostty
//! (kitty's), iTerm2, WezTerm, VS Code and the others `ratatui-image` lists
//! (iTerm2's); everywhere else, and in a terminal that does not report its
//! pixel size (agent-mux's own panes report none), in Unicode half blocks,
//! two pixels a cell. No escape is written to ask the terminal, so nothing
//! competes with the event reader. `$AGENT_MUX_IMAGES` picks one:
//! `kitty`, `iterm2`, `sixel`, `halfblocks`, or `off` for captions only.

use ratatui::layout::Size;
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const ENV: &str = "AGENT_MUX_IMAGES";

/// The most rows one image takes; a taller one is scaled down.
const MAX_ROWS: u16 = 30;

/// Where an image is drawn in a rendered document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSpot {
    /// The first of its rows.
    pub line: usize,
    /// Its first column (past the quotes and list markers it sits in).
    pub col: usize,
    pub rows: u16,
    pub cols: u16,
    pub path: PathBuf,
}

/// What the renderer needs to place images: the folder relative paths
/// resolve against, and the size of a cell in pixels.
#[derive(Debug, Clone)]
pub struct Images {
    pub base: PathBuf,
    pub cell: (u16, u16),
}

impl Images {
    /// For a document in `base`, at the terminal's cell size; `None` when
    /// images are turned off.
    pub fn for_dir(base: &Path) -> Option<Images> {
        let picker = picker()?;
        let FontSize { width, height } = picker.font_size();
        Some(Images {
            base: base.to_path_buf(),
            cell: (width.max(1), height.max(1)),
        })
    }

    /// The local image file a link names, when there is one.
    pub fn resolve(&self, url: &str) -> Option<PathBuf> {
        let url = url.trim();
        let url = url.strip_prefix("file://").unwrap_or(url);
        if url.is_empty() || url.starts_with('#') || url.contains("://") || url.starts_with("data:")
        {
            return None;
        }
        let url = url.split(['#', '?']).next().unwrap_or(url);
        let path = self.base.join(url);
        path.is_file().then_some(path)
    }

    /// The cells `path` takes at most `avail` columns wide: its natural
    /// size, scaled down to fit the width and `MAX_ROWS`.
    pub fn size(&self, path: &Path, avail: usize) -> Option<(u16, u16)> {
        let (w, h) = dimensions(path)?;
        let (cw, ch) = (self.cell.0 as f64, self.cell.1 as f64);
        let mut cols = (w as f64 / cw).max(1.0);
        let mut rows = (h as f64 / ch).max(1.0);
        let fit = (avail.max(1) as f64 / cols)
            .min(MAX_ROWS as f64 / rows)
            .min(1.0);
        cols = (cols * fit).round().max(1.0);
        rows = (rows * fit).round().max(1.0);
        Some((cols as u16, rows as u16))
    }
}

fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// An image's size in pixels.
fn dimensions(path: &Path) -> Option<(u32, u32)> {
    if is_svg(path) {
        let size = svg_tree(path, false)?.size();
        return Some((size.width().ceil() as u32, size.height().ceil() as u32));
    }
    image::image_dimensions(path).ok()
}

/// The system's fonts, loaded once, for an SVG's text.
fn fonts() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            std::sync::Arc::new(db)
        })
        .clone()
}

/// An SVG parsed, with the system's fonts when its text will be drawn.
fn svg_tree(path: &Path, with_text: bool) -> Option<resvg::usvg::Tree> {
    let data = std::fs::read(path).ok()?;
    let mut opt = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    if with_text {
        opt.fontdb = fonts();
    }
    resvg::usvg::Tree::from_data(&data, &opt).ok()
}

/// An SVG drawn over white into `width` × `height` pixels, kept in
/// proportion and centred.
fn rasterize(path: &Path, width: u32, height: u32) -> Option<image::DynamicImage> {
    let tree = svg_tree(path, true)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width.max(1), height.max(1))?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let size = tree.size();
    let scale = (width as f32 / size.width()).min(height as f32 / size.height());
    let (dx, dy) = (
        (width as f32 - size.width() * scale) / 2.0,
        (height as f32 - size.height() * scale) / 2.0,
    );
    let transform = resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // opaque over white, so the premultiplied pixels are plain RGBA
    let (w, h) = (pixmap.width(), pixmap.height());
    image::RgbaImage::from_raw(w, h, pixmap.take()).map(image::DynamicImage::ImageRgba8)
}

/// How pictures are drawn in this terminal, once per process; `None` when
/// `$AGENT_MUX_IMAGES=off`.
pub fn picker() -> Option<&'static Picker> {
    static PICKER: OnceLock<Option<Picker>> = OnceLock::new();
    PICKER
        .get_or_init(|| {
            let env = |k: &str| std::env::var(k).unwrap_or_default();
            let choice = env(ENV).to_ascii_lowercase();
            let pixels = crossterm::terminal::window_size()
                .ok()
                .filter(|w| w.width > 0 && w.height > 0 && w.columns > 0 && w.rows > 0)
                .map(|w| FontSize::new(w.width / w.columns, w.height / w.rows));
            let protocol = match choice.as_str() {
                "off" | "none" | "0" => return None,
                "" => detect(&env("TERM"), &env("TERM_PROGRAM"), &env("LC_TERMINAL"))
                    .filter(|_| pixels.is_some()),
                c => named(c),
            };
            #[allow(deprecated)] // the documented way without querying stdin
            let mut p = Picker::from_fontsize(pixels.unwrap_or(FontSize::new(10, 20)));
            p.set_protocol_type(protocol.unwrap_or(ProtocolType::Halfblocks));
            Some(p)
        })
        .as_ref()
}

fn named(name: &str) -> Option<ProtocolType> {
    match name {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        _ => None,
    }
}

/// The graphics protocol a terminal speaks, from its environment.
fn detect(term: &str, program: &str, lc_terminal: &str) -> Option<ProtocolType> {
    let program = program.to_ascii_lowercase();
    if term == "xterm-kitty" || term == "xterm-ghostty" || program == "ghostty" {
        return Some(ProtocolType::Kitty);
    }
    let iterm2 = [
        "iterm",
        "wezterm",
        "mintty",
        "vscode",
        "tabby",
        "hyper",
        "rio",
        "warpterminal",
    ];
    if iterm2.iter().any(|p| program.contains(p)) || lc_terminal.contains("iTerm") {
        return Some(ProtocolType::Iterm2);
    }
    None
}

/// The picture for a spot, encoded for the terminal; `None` when the file
/// cannot be decoded.
pub fn encode(spot: &ImageSpot) -> Option<SlicedProtocol> {
    let picker = picker()?;
    let img = if is_svg(&spot.path) {
        let FontSize { width, height } = picker.font_size();
        rasterize(
            &spot.path,
            spot.cols as u32 * width.max(1) as u32,
            spot.rows as u32 * height.max(1) as u32,
        )?
    } else {
        image::ImageReader::open(&spot.path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?
    };
    SlicedProtocol::new(picker, img, Some(Size::new(spot.cols, spot.rows))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_comes_from_the_terminal_it_runs_in() {
        assert_eq!(detect("xterm-kitty", "", ""), Some(ProtocolType::Kitty));
        assert_eq!(
            detect("xterm-ghostty", "ghostty", ""),
            Some(ProtocolType::Kitty)
        );
        assert_eq!(
            detect("xterm-256color", "iTerm.app", ""),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(
            detect("xterm-256color", "WezTerm", ""),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(detect("xterm-256color", "Apple_Terminal", ""), None);
        assert_eq!(named("sixel"), Some(ProtocolType::Sixel));
        assert_eq!(named("halfblocks"), None);
    }

    #[test]
    fn only_local_files_resolve_and_sizes_fit_the_width() {
        let d = tempfile::tempdir().unwrap();
        let png = d.path().join("a.png");
        image::RgbImage::new(200, 100).save(&png).unwrap();
        let imgs = Images {
            base: d.path().to_path_buf(),
            cell: (10, 20),
        };
        assert_eq!(imgs.resolve("a.png"), Some(png.clone()));
        assert_eq!(imgs.resolve("./a.png#x"), Some(d.path().join("./a.png")));
        assert_eq!(imgs.resolve("https://x.org/a.png"), None);
        assert_eq!(imgs.resolve("missing.png"), None);
        assert_eq!(imgs.size(&png, 80), Some((20, 5)));
        assert_eq!(imgs.size(&png, 10), Some((10, 3)), "scaled to the width");
        let tall = d.path().join("t.png");
        image::RgbImage::new(10, 2000).save(&tall).unwrap();
        assert_eq!(imgs.size(&tall, 80), Some((1, MAX_ROWS)));
        let svg = d.path().join("s.svg");
        std::fs::write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200"><rect x="0" y="0" width="200" height="200" fill="#ff0000"/></svg>"##,
        )
        .unwrap();
        assert_eq!(imgs.size(&svg, 80), Some((40, 10)), "an SVG's own size");
        let img = rasterize(&svg, 40, 20).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(5, 10).0, [255, 0, 0, 255], "drawn");
        assert_eq!(img.get_pixel(35, 10).0, [255, 255, 255, 255], "over white");
        std::fs::write(d.path().join("n.png"), "not an image").unwrap();
        assert_eq!(imgs.size(&d.path().join("n.png"), 80), None);
    }
}
