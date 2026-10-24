//! A message as it was sent, drawn by Blitz.
//!
//! Blitz lays the message out as a browser would, and only the part in view
//! is painted, into a picture the reading pane shows. Pictures, style sheets
//! and fonts come through the engine: inline ones always, those on the
//! internet only if the user allowed them for the message. Scripts never
//! run. Blitz is a beta, so every call into it is guarded: should it panic,
//! the page is dropped and the message is drawn natively instead.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{DocumentConfig, Point, StyleThreading, UserAgentStylesheet, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request as NetRequest};
use blitz_traits::shell::{ColorScheme, Viewport};
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
    renderer: Option<(u32, u32, VelloCpuImageRenderer)>,
    /// The viewport laid out for: physical width and height, and the scale.
    viewport: (u32, u32, f32),
    arrived: Arc<AtomicBool>,
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
        Self { doc: HtmlDocument::from_html(html, config), renderer: None, viewport: (0, 0, 0.0), arrived }
    }

    /// Lays the page out for a view of `width` by `height` logical pixels.
    pub fn view(&mut self, width: f32, height: f32, scale: f32) {
        let viewport = ((width * scale).round().max(1.0) as u32, (height * scale).round().max(1.0) as u32, scale);
        if viewport != self.viewport {
            self.viewport = viewport;
            self.doc.set_viewport(Viewport::new(viewport.0, viewport.1, scale, ColorScheme::Light));
        }
        self.doc.resolve(0.0);
    }

    /// Takes in pictures, style sheets and fonts that have arrived.
    pub fn load(&mut self) {
        self.arrived.store(false, Ordering::Release);
        self.doc.handle_messages();
        self.doc.resolve(0.0);
    }

    /// The size of everything on the page, in logical pixels.
    pub fn size(&self) -> (f32, f32) {
        let layout = self.doc.root_element().final_layout();
        let width = layout.size.width + layout.scroll_width();
        let height = layout.size.height + layout.scroll_height();
        (width, height.min(MAX_HEIGHT))
    }

    /// Paints the part of the page in view, scrolled by `x`, `y` logical
    /// pixels.
    pub fn paint(&mut self, x: f32, y: f32) -> Image {
        let (width, height, scale) = self.viewport;
        if !matches!(self.renderer, Some((w, h, _)) if w == width && h == height) {
            self.renderer = Some((width, height, VelloCpuImageRenderer::new(width, height)));
        }
        let Some((_, _, renderer)) = self.renderer.as_mut() else { unreachable!() };
        self.doc.set_viewport_scroll(Point { x: f64::from(x), y: f64::from(y) });
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
        let doc = &mut self.doc;
        renderer.render(
            |scene| blitz_paint::paint_scene(scene, doc, f64::from(scale), width, height, 0, 0),
            pixels.make_mut_bytes(),
        );
        Image::from_rgba8_premultiplied(pixels)
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
        let picture = page.paint(0.0, 500.0);
        assert_eq!((picture.size().width, picture.size().height), (400, 300));
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
