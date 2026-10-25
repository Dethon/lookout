//! A message as it was sent, drawn by Blitz.
//!
//! Blitz lays the message out as a browser would, and only the part in view
//! and a little around it is painted, into a picture the reading pane
//! scrolls; another is painted only when the view leaves it. Text can be
//! selected with the mouse, as in a browser. Pictures, style sheets
//! and fonts come through the engine: inline ones always, those on the
//! internet only if the user allowed them for the message. Scripts never
//! run. Blitz is a beta, so every call into it is guarded: should it panic,
//! the page is dropped and the message is drawn natively instead.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyrender::{ImageRenderer, PaintScene};
use anyrender_vello_cpu::{ImageCacheConfig, VelloCpuImageRenderer};
use blitz_dom::{DocumentConfig, NodeId, Point, StyleThreading, UserAgentStylesheet, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request as NetRequest};
use blitz_traits::shell::{ColorScheme, Viewport};
use peniko::color::palette::css::WHITE;
use peniko::kurbo::{Affine, Rect};
use peniko::Fill;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::engine::{Engine, Request};

// What browsers do with presentational attributes that Blitz doesn't yet.
const EMAIL_CSS: &str = "
table[align=center i] { margin-left: auto; margin-right: auto; }
td[align=center i] > table, th[align=center i] > table, center > table, div[align=center i] > table {
    margin-left: auto; margin-right: auto;
}
td[align=right i] > table, div[align=right i] > table { margin-left: auto; }
";

// Past this, the rest of a very long message isn't laid out at all.
const MAX_HEIGHT: f32 = 60_000.0;

// The picture reaches this share of the view's height past it, above and
// below, so that scrolling moves it rather than painting another.
const MARGIN: f32 = 0.5;

// The renderer keeps its own copy of the pictures it draws, up to this many
// bytes beyond those in view; the default is 64 MB.
const PICTURE_CACHE: usize = 32 * 1024 * 1024;

/// Runs a call into Blitz, catching a panic. A panic leaves the document in
/// an unknown state, so the caller drops it.
pub fn guard<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            log::error!("Blitz panicked {what}: {message}");
            None
        }
    }
}

pub struct Page {
    doc: HtmlDocument,
    /// The renderer, and the physical size it draws.
    renderer: Option<((u32, u32), VelloCpuImageRenderer)>,
    /// The viewport laid out for: physical width and height, and the scale.
    viewport: (u32, u32, f32),
    /// The part of the page the picture shown holds, in logical pixels: x,
    /// y, width and height. None once it is out of date.
    painted: Option<(f32, f32, f32, f32)>,
    /// A selection is being dragged and has found its first letter.
    anchored: bool,
    arrived: Arc<AtomicBool>,
}

/// A picture of part of the page, and where it goes, in logical pixels.
pub struct Painted {
    pub picture: Image,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Page {
    pub fn new(html: &str, resources: Resources) -> Self {
        let arrived = resources.arrived.clone();
        let config = DocumentConfig {
            ua_stylesheets: vec![UserAgentStylesheet::default_sheet(), UserAgentStylesheet::parse(EMAIL_CSS)],
            net_provider: Some(Arc::new(resources)),
            // One document on one thread: no thread pool to keep.
            style_threading: StyleThreading::Sequential,
            ..Default::default()
        };
        Self {
            doc: HtmlDocument::from_html(html, config),
            renderer: None,
            viewport: (0, 0, 0.0),
            painted: None,
            anchored: false,
            arrived,
        }
    }

    /// Lays the page out for a view of `width` by `height` logical pixels.
    pub fn view(&mut self, width: f32, height: f32, scale: f32) {
        let viewport = ((width * scale).round().max(1.0) as u32, (height * scale).round().max(1.0) as u32, scale);
        if viewport != self.viewport {
            self.viewport = viewport;
            self.painted = None;
            self.doc.set_viewport(Viewport::new(viewport.0, viewport.1, scale, ColorScheme::Light));
        }
        self.doc.resolve(0.0);
    }

    /// Takes in pictures, style sheets and fonts that have arrived.
    pub fn load(&mut self) {
        self.arrived.store(false, Ordering::Release);
        self.doc.handle_messages();
        self.doc.resolve(0.0);
        self.painted = None;
    }

    /// The size of everything on the page, in logical pixels.
    pub fn size(&self) -> (f32, f32) {
        let layout = self.doc.root_element().final_layout();
        let width = layout.size.width + layout.scroll_width();
        let height = layout.size.height + layout.scroll_height();
        (width, height.min(MAX_HEIGHT))
    }

    /// Paints the view scrolled by `x`, `y` logical pixels, and some way
    /// above and below it, so that scrolling moves the picture instead of
    /// painting another. None if the picture shown still holds the view.
    pub fn paint(&mut self, x: f32, y: f32) -> Option<Painted> {
        let (width, height, scale) = self.viewport;
        let (view_width, view_height) = (width as f32 / scale, height as f32 / scale);
        if let Some((px, py, _, ph)) = self.painted
            && px == x
            && py <= y
            && y + view_height <= py + ph
        {
            return None;
        }
        let margin = (view_height * MARGIN).round();
        let band = (view_height + 2.0 * margin).min(self.size().1.max(view_height));
        let top = (y - margin).min(self.size().1 - band).max(0.0);
        let band_height = ((band * scale).round() as u32).max(height);
        let (size, renderer) = self.renderer.get_or_insert_with(|| {
            let cache = ImageCacheConfig { max_bytes: PICTURE_CACHE, ..Default::default() };
            ((width, band_height), VelloCpuImageRenderer::with_image_cache_config(width, band_height, cache))
        });
        if *size != (width, band_height) {
            // Keeps the pictures it has converted.
            *size = (width, band_height);
            renderer.resize(width, band_height);
        }
        // The renderer keeps what it was told to draw until reset: without
        // this, every paint drew all the earlier ones again, slower each
        // time, and pictures since forgotten made it panic.
        renderer.reset();
        self.doc.set_viewport_scroll(Point { x: f64::from(x), y: f64::from(top) });
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, band_height);
        let doc = &mut self.doc;
        renderer.render(
            |scene| {
                // On white, as messages expect: the picture is opaque, and
                // Slint copies it to the screen without blending.
                let all = Rect::new(0.0, 0.0, f64::from(width), f64::from(band_height));
                scene.fill(Fill::NonZero, Affine::IDENTITY, WHITE, None, &all);
                blitz_paint::paint_scene(scene, doc, f64::from(scale), width, band_height, 0, 0);
            },
            pixels.make_mut_bytes(),
        );
        self.painted = Some((x, top, view_width, band));
        Some(Painted { picture: Image::from_rgba8(pixels), x, y: top, width: view_width, height: band })
    }

    /// The link at `x`, `y` logical pixels into the page.
    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let mut node = self.doc.get_node(self.doc.hit(x, y)?.node_id);
        while let Some(current) = node {
            if current.element_data().is_some_and(|element| element.name.local == local_name!("a")) {
                return current.attr(local_name!("href")).map(|href| href.trim().to_owned()).filter(|href| !href.is_empty());
            }
            node = current.parent.and_then(|parent| self.doc.get_node(parent));
        }
        None
    }

    /// There is text at `x`, `y` logical pixels into the page.
    pub fn text_at(&self, x: f32, y: f32) -> bool {
        self.doc.find_text_position(x, y).is_some()
    }

    /// Starts selecting text at `x`, `y`, clearing what was selected. True
    /// if the page must be painted again.
    pub fn select_from(&mut self, x: f32, y: f32) -> bool {
        let had = self.doc.has_text_selection();
        self.doc.clear_text_selection();
        self.anchored = false;
        if let Some((node, offset)) = self.doc.find_text_position(x, y) {
            self.doc.set_text_selection(node, offset, node, offset);
            self.anchored = true;
        }
        self.repaint_if(had)
    }

    /// Takes the selection being dragged to `x`, `y`. Started away from
    /// text, it starts at the first letter reached. True if the page must
    /// be painted again.
    pub fn select_to(&mut self, x: f32, y: f32) -> bool {
        if !self.anchored {
            if let Some((node, offset)) = self.doc.find_text_position(x, y) {
                self.doc.set_text_selection(node, offset, node, offset);
                self.anchored = true;
            }
            return false;
        }
        let before = self.doc.get_text_selection_ranges();
        self.doc.extend_text_selection_to_point(x, y);
        let changed = self.doc.get_text_selection_ranges() != before;
        self.repaint_if(changed)
    }

    /// Selects the word at `x`, `y`. True if the page must be painted again.
    pub fn select_word(&mut self, x: f32, y: f32) -> bool {
        let Some((node, offset)) = self.doc.find_text_position(x, y) else { return false };
        let Some(text) = self.inline_text(node) else { return false };
        let (start, end) = word_around(text, offset);
        if start == end {
            return false;
        }
        self.doc.set_text_selection(node, start, node, end);
        self.anchored = false;
        self.repaint_if(true)
    }

    /// The selected text, a line for each block it spans.
    pub fn selected_text(&self) -> Option<String> {
        let lines: Vec<&str> = self
            .doc
            .get_text_selection_ranges()
            .into_iter()
            .filter_map(|(node, start, end)| self.inline_text(node)?.get(start..end))
            .collect();
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    fn inline_text(&self, node: NodeId) -> Option<&str> {
        let layout = self.doc.get_node(node)?.element_data()?.inline_layout_data.as_ref()?;
        Some(layout.text.as_str())
    }

    fn repaint_if(&mut self, changed: bool) -> bool {
        if changed {
            self.painted = None;
        }
        changed
    }
}

/// The word around byte `at` of `text`, or the one character there if it
/// is not part of a word.
fn word_around(text: &str, at: usize) -> (usize, usize) {
    let at = (0..=at.min(text.len())).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '\u{2019}';
    let after = text[at..].chars().next();
    let before = text[..at].chars().next_back();
    if !after.is_some_and(word) {
        if before.is_some_and(word) {
            // Just past the end of a word, as clicking its right half finds.
            let start = text[..at].char_indices().rev().take_while(|&(_, c)| word(c)).last().map_or(at, |(i, _)| i);
            return (start, at);
        }
        return after.map_or((at, at), |c| (at, at + c.len_utf8()));
    }
    let start = text[..at].char_indices().rev().take_while(|&(_, c)| word(c)).last().map_or(at, |(i, _)| i);
    let end = text[at..].char_indices().find(|&(_, c)| !word(c)).map_or(text.len(), |(i, _)| at + i);
    (start, end)
}

/// Fetches a page's pictures, style sheets and fonts through the engine.
pub struct Resources {
    engine: Engine,
    account: String,
    message: String,
    /// Pictures and other things from the internet may load.
    remote: bool,
    /// Something has arrived that the page hasn't taken in yet.
    arrived: Arc<AtomicBool>,
    /// Called on the engine's thread when something first arrives.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Resources {
    pub fn new(engine: Engine, account: String, message: String, remote: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { engine, account, message, remote, arrived: Arc::default(), wake: Arc::new(wake) }
    }
}

impl NetProvider for Resources {
    fn fetch(&self, _doc_id: usize, request: NetRequest, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        let arrived = self.arrived.clone();
        let wake = self.wake.clone();
        self.engine.send(Request::Resource {
            account: self.account.clone(),
            message: self.message.clone(),
            url: url.clone(),
            remote: self.remote,
            reply: Box::new(move |bytes| {
                let Some(bytes) = bytes else { return };
                // Pictures are decoded and style sheets parsed here, off the
                // interface's thread.
                if guard("taking a resource", || handler.bytes(url, Bytes::from(bytes))).is_none() {
                    return;
                }
                if !arrived.swap(true, Ordering::AcqRel) {
                    wake();
                }
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(html: &str) -> Page {
        let resources = Resources::new(Engine::idle(), String::new(), String::new(), false, || {});
        let mut page = Page::new(html, resources);
        page.view(400.0, 300.0, 1.0);
        page
    }

    #[test]
    fn paints_what_is_in_view() {
        let mut page = page("<body style=\"margin:0\"><div style=\"height:1000px;background:#ff0000\"></div></body>");
        let (width, height) = page.size();
        assert_eq!(width, 400.0);
        assert!(height >= 1000.0, "{height}");
        let painted = page.paint(0.0, 500.0).unwrap();
        // Half the view's height beyond it, above and below.
        assert_eq!((painted.x, painted.y, painted.width, painted.height), (0.0, 350.0, 400.0, 600.0));
        assert_eq!((painted.picture.size().width, painted.picture.size().height), (400, 600));
    }

    #[test]
    fn scrolling_within_the_picture_paints_nothing() {
        let mut page = page("<body style=\"margin:0\"><div style=\"height:3000px\"></div></body>");
        assert!(page.paint(0.0, 1000.0).is_some());
        assert!(page.paint(0.0, 1100.0).is_none());
        assert!(page.paint(0.0, 850.0).is_none());
        assert!(page.paint(0.0, 1200.0).is_some(), "past the picture");
        page.view(400.0, 320.0, 1.0);
        assert!(page.paint(0.0, 1200.0).is_some(), "a new size");
    }

    #[test]
    fn the_end_of_the_page_is_painted_once() {
        let mut page = page("<body style=\"margin:0\"><div style=\"height:1000px\"></div></body>");
        let painted = page.paint(0.0, 700.0).unwrap();
        assert_eq!(painted.y + painted.height, 1000.0);
        assert!(page.paint(0.0, 700.0).is_none());
    }

    #[test]
    fn a_short_page_is_painted_whole() {
        let mut page = page("<body style=\"margin:0\"><p>Hi</p></body>");
        let painted = page.paint(0.0, 0.0).unwrap();
        assert_eq!((painted.y, painted.height), (0.0, 300.0));
    }

    #[test]
    fn pictures_are_opaque_on_white() {
        let mut page = page("<body style=\"margin:0\"><div style=\"height:10px;background:rgba(255,0,0,0.5)\"></div></body>");
        let picture = page.paint(0.0, 0.0).unwrap().picture.to_rgba8().unwrap();
        let (top, below) = (picture.as_slice()[0], picture.as_slice()[400 * 20]);
        assert_eq!((top.r, top.a, below.r, below.g, below.b, below.a), (255, 255, 255, 255, 255, 255));
        assert!((126..=129).contains(&top.g), "{top:?}");
    }

    const TEXT: &str = "<body style=\"margin:0;font-size:20px;line-height:30px\">\
        <p style=\"margin:0\">Hello there world</p><p style=\"margin:0\">Second line</p></body>";

    #[test]
    fn dragging_selects_text() {
        let mut page = page(TEXT);
        page.paint(0.0, 0.0);
        assert!(page.text_at(5.0, 15.0));
        assert!(!page.text_at(5.0, 200.0));
        assert!(!page.select_from(1.0, 15.0));
        assert!(page.select_to(390.0, 45.0));
        assert!(page.paint(0.0, 0.0).is_some(), "the selection is painted");
        assert_eq!(page.selected_text().as_deref(), Some("Hello there world\nSecond line"));
        assert!(page.select_from(1.0, 15.0), "a new press clears it");
        assert_eq!(page.selected_text(), None);
    }

    #[test]
    fn a_drag_from_beside_the_text_starts_at_the_first_letter() {
        let mut page = page(TEXT);
        page.select_from(5.0, 250.0);
        page.select_to(1.0, 45.0);
        page.select_to(390.0, 45.0);
        assert_eq!(page.selected_text().as_deref(), Some("Second line"));
    }

    #[test]
    fn double_clicking_selects_a_word() {
        let mut page = page(TEXT);
        let x = (1..200).map(|x| x as f32).find(|&x| page.select_word(x, 15.0) && page.selected_text().as_deref() == Some("there"));
        assert!(x.is_some_and(|x| x > 20.0), "{x:?}");
        assert_eq!(word_around("Hello there", 7), (6, 11));
        assert_eq!(word_around("Hello there", 5), (0, 5));
        assert_eq!(word_around("a, b", 1), (0, 1));
        assert_eq!(word_around("a,  b", 2), (2, 3));
        assert_eq!(word_around("don't", 0), (0, 5));
        assert_eq!(word_around("", 0), (0, 0));
    }

    #[test]
    fn a_paint_shows_only_what_is_in_view_now() {
        let mut page = page("<body style=\"margin:0\"><div style=\"height:200px;background:#ff0000\"></div><div style=\"height:3000px\"></div></body>");
        page.paint(0.0, 0.0);
        let picture = page.paint(0.0, 2000.0).unwrap().picture.to_rgba8().unwrap();
        let first = picture.as_slice()[0];
        assert_ne!((first.r, first.g, first.b), (255, 0, 0), "an earlier paint shows through");
    }

    #[test]
    fn pictures_scrolled_away_dont_break_later_paints() {
        let (engine, mut requests) = Engine::queued();
        let resources = Resources::new(engine, String::new(), String::new(), true, || {});
        let mut page = Page::new(
            "<body style=\"margin:0\"><img src=\"https://example.com/a.png\" width=\"100\" height=\"100\">\
             <div style=\"height:3000px\"></div></body>",
            resources,
        );
        page.view(400.0, 300.0, 1.0);
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 255, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        while let Ok(request) = requests.try_recv() {
            if let Request::Resource { reply, .. } = request {
                reply(Some(png.get_ref().clone()));
            }
        }
        page.load();
        page.paint(0.0, 0.0);
        // Long enough for the renderer to forget the picture.
        for frame in 0..100 {
            page.paint(0.0, 1000.0 + frame as f32);
        }
    }

    #[test]
    fn finds_links_where_they_are_painted() {
        let mut page = page(
            "<body style=\"margin:0\"><div style=\"height:600px\"></div>\
             <a href=\" https://example.com/x \" style=\"display:block;height:40px\">here</a></body>",
        );
        page.paint(0.0, 500.0);
        assert_eq!(page.link_at(10.0, 610.0).as_deref(), Some("https://example.com/x"));
        assert_eq!(page.link_at(10.0, 550.0), None);
    }

    #[test]
    fn a_panic_is_caught() {
        assert_eq!(guard("testing", || 1), Some(1));
        assert_eq!(guard("testing", || -> i32 { panic!("boom") }), None);
    }
}
