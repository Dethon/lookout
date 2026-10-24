//! A message as it was sent, for when the native rendering falls short.
//!
//! On Windows the reading pane can switch to WebView2, embedded in the main
//! window. It is only created while the switch is on, and closed when it is
//! turned off, so its memory is given back. Elsewhere the message opens in
//! the default browser. Scripts never run; pictures, style sheets and fonts
//! from the internet only load if the user allowed them for the message.

use std::path::{Path, PathBuf};

/// Writes the page for a message into its own folder, with the inline
/// pictures beside it. `pictures` maps each `cid:` reference to its file
/// name and bytes.
pub fn write_page(
    dir: &Path,
    html: &str,
    pictures: &[(String, String, Vec<u8>)],
    remote_pictures: bool,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut html = html.to_owned();
    for (reference, file, bytes) in pictures {
        std::fs::write(dir.join(file), bytes)?;
        html = replace_ignoring_case(&html, reference, file);
    }
    // Remote style sheets and fonts track as well as pictures do, so they
    // follow the same choice.
    let remote = if remote_pictures { "*" } else { "'self'" };
    // Before everything else, so the parser puts it in the head and it
    // applies to all that follows.
    let policy = format!(
        "<meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; style-src 'unsafe-inline' {remote} file:; font-src {remote} file:; \
         img-src {remote} data: file:\">"
    );
    let path = dir.join("message.html");
    std::fs::write(&path, format!("{policy}{html}"))?;
    Ok(path)
}

fn replace_ignoring_case(text: &str, from: &str, to: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(&needle) {
        out.push_str(&text[last..at]);
        out.push_str(to);
        last = at + needle.len();
    }
    out.push_str(&text[last..]);
    out
}

/// Opens the page in the default browser.
#[cfg(not(windows))]
pub fn open_in_browser(path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| e.to_string())
}

/// The WebView2 panel over the reading pane's body.
#[cfg(windows)]
pub struct Embedded {
    context: wry::WebContext,
    webview: Option<wry::WebView>,
}

#[cfg(windows)]
impl Embedded {
    pub fn new() -> Self {
        // WebView2 keeps its profile in the data folder, not beside the
        // program, which may be installed where the user can't write.
        let profile = lookout_core::config::data_dir().ok().map(|dir| dir.join("webview"));
        Self { context: wry::WebContext::new(profile), webview: None }
    }

    fn rect(bounds: (f32, f32, f32, f32)) -> wry::Rect {
        let (x, y, width, height) = bounds;
        wry::Rect {
            position: winit::dpi::LogicalPosition::new(f64::from(x), f64::from(y)).into(),
            size: winit::dpi::LogicalSize::new(f64::from(width.max(1.0)), f64::from(height.max(1.0))).into(),
        }
    }

    /// Shows `path` in the panel, creating it on first use. `bounds` are
    /// logical pixels in the window.
    pub fn show(&mut self, window: &slint::Window, path: &Path, bounds: (f32, f32, f32, f32)) -> Result<(), String> {
        let url = format!("file:///{}", path.to_string_lossy().replace('\\', "/"));
        if let Some(webview) = &self.webview {
            webview.load_url(&url).map_err(|e| e.to_string())?;
            webview.set_bounds(Self::rect(bounds)).map_err(|e| e.to_string())?;
            return webview.set_visible(true).map_err(|e| e.to_string());
        }
        let handle = window.window_handle();
        let webview = wry::WebViewBuilder::new_with_web_context(&mut self.context)
            .with_url(&url)
            .with_bounds(Self::rect(bounds))
            .with_devtools(false)
            // Links open in the browser; the page itself never navigates.
            .with_navigation_handler(|url| {
                if url.starts_with("file:") || url.starts_with("about:") {
                    return true;
                }
                if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                    _ = open::that_detached(&url);
                }
                false
            })
            .with_new_window_req_handler(|url, _| {
                if url.starts_with("http://") || url.starts_with("https://") {
                    _ = open::that_detached(&url);
                }
                wry::NewWindowResponse::Deny
            })
            .build_as_child(&handle)
            .map_err(|e| e.to_string())?;
        self.webview = Some(webview);
        Ok(())
    }

    pub fn set_bounds(&self, bounds: (f32, f32, f32, f32)) {
        if let Some(webview) = &self.webview {
            _ = webview.set_bounds(Self::rect(bounds));
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if let Some(webview) = &self.webview {
            _ = webview.set_visible(visible);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_pictures_point_at_their_files() {
        let dir = std::env::temp_dir().join(format!("lookout-viewer-test-{}", std::process::id()));
        let pictures = vec![("cid:Logo@X".to_owned(), "inline-0.png".to_owned(), vec![1, 2, 3])];
        let path = write_page(&dir, "<p><img src=\"cid:logo@x\"></p>", &pictures, false).unwrap();
        let page = std::fs::read_to_string(&path).unwrap();
        assert!(page.contains("src=\"inline-0.png\""));
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("img-src 'self'"));
        assert_eq!(std::fs::read(dir.join("inline-0.png")).unwrap(), [1, 2, 3]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
