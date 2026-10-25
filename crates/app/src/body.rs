//! A message body as the reading pane's blocks.

use std::fmt::Write;

use lookout_core::html::{self, BlockKind, ImageRef, Rendered};
use slint::{Image, ModelRc, StyledText, VecModel};

use crate::{BodyBlock, Picture};

// Pictures without a size wait at this one until they arrive.
const WAITING_WIDTH: f32 = 140.0;
const WAITING_HEIGHT: f32 = 28.0;
// Between pictures in a row.
const GAP: f32 = 4.0;

pub fn blocks(rendered: &Rendered, dark: bool) -> Vec<BodyBlock> {
    rendered
        .blocks
        .iter()
        .map(|block| {
            let text = match block.kind {
                BlockKind::Paragraph | BlockKind::Heading(_) => styled(&readable_colors(&block.markdown, dark), &block.plain),
                _ => StyledText::default(),
            };
            let pictures: Vec<Picture> = block
                .images
                .iter()
                .map(|image| {
                    let (width, height) = waiting_size(image.width, image.height);
                    Picture {
                        image: Image::default(),
                        loaded: false,
                        alt: image.alt.as_str().into(),
                        link: image.link.as_deref().unwrap_or_default().into(),
                        x: 0.0,
                        width,
                        height,
                    }
                })
                .collect();
            let mut body = BodyBlock {
                kind: match block.kind {
                    BlockKind::Paragraph => 0,
                    BlockKind::Heading(_) => 1,
                    BlockKind::Rule => 2,
                    BlockKind::Image => 3,
                },
                text,
                level: match block.kind {
                    BlockKind::Heading(level) => i32::from(level),
                    _ => 0,
                },
                indent: i32::from(block.indent),
                quote: i32::from(block.quote),
                mono: block.mono,
                pictures: ModelRc::default(),
                row_width: 0.0,
                row_height: 0.0,
            };
            set_pictures(&mut body, pictures);
            body
        })
        .collect()
}

/// The theme's colours, as in `theme.slint`, for the simplified view drawn
/// by Blitz.
pub struct Colours {
    pub surface: &'static str,
    pub surface_alt: &'static str,
    pub text: &'static str,
    pub faint: &'static str,
    pub link: &'static str,
    pub quote_bar: &'static str,
    pub divider: &'static str,
    /// Over selected text, translucent.
    pub selection: (u8, u8, u8, u8),
}

pub fn colours(dark: bool) -> Colours {
    if dark {
        Colours {
            surface: "#242427",
            surface_alt: "#2b2b2f",
            text: "#f1f1f3",
            faint: "#808088",
            link: "#8ab4f8",
            quote_bar: "#5a5a62",
            divider: "rgba(255,255,255,0.08)",
            selection: (0x6a, 0xa5, 0xf5, 0x66),
        }
    } else {
        Colours {
            surface: "#ffffff",
            surface_alt: "#fafafa",
            text: "#1f1f1f",
            faint: "#8a8a8a",
            link: "#0f6cbd",
            quote_bar: "#c8c8c8",
            divider: "rgba(0,0,0,0.08)",
            selection: (0x0f, 0x6c, 0xbd, 0x40),
        }
    }
}

/// The simplified view as a page for Blitz to draw, so that its text can
/// be selected: the blocks laid out as the native view lays them out, in
/// the theme's colours. Pictures from the internet show as their
/// description unless `remote`.
pub fn page(rendered: &Rendered, dark: bool, remote: bool) -> String {
    let c = colours(dark);
    let mut out = format!(
        "<!DOCTYPE html><html><head><style>\
         html, body {{ margin: 0; background: {surface}; color: {text}; }}\
         body {{ font-family: \"Segoe UI\", system-ui, sans-serif; font-size: 14px; padding: 4px 0 12px; }}\
         a, a span {{ color: {link} !important; }}\
         .b {{ position: relative; padding: 3px 24px 5px 0; overflow-wrap: anywhere; }}\
         .h {{ padding-top: 10px; }}\
         .r {{ padding: 8px 24px 8px 0; }}\
         .r hr {{ border: 0; border-top: 1px solid {divider}; margin: 0; }}\
         .m {{ font-family: Consolas, monospace; white-space: pre-wrap; }}\
         .q {{ position: absolute; top: 0; bottom: 0; width: 3px; background: {quote_bar}; }}\
         .row {{ display: flex; gap: {GAP}px; align-items: flex-start; max-width: 100%; }}\
         .row > * {{ display: block; min-width: 0; }}\
         .row img {{ display: block; max-width: 100%; }}\
         .alt {{ box-sizing: border-box; border: 1px solid {divider}; background: {surface_alt}; color: {faint};\
                 font-size: 12px; padding: 5px; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }}\
         a .alt {{ color: {link}; }}\
         </style></head><body>",
        surface = c.surface,
        text = c.text,
        link = c.link,
        divider = c.divider,
        quote_bar = c.quote_bar,
        surface_alt = c.surface_alt,
        faint = c.faint,
    );
    for block in &rendered.blocks {
        let left = 20 + u32::from(block.indent) * 18 + u32::from(block.quote) * 14;
        let class = match block.kind {
            BlockKind::Heading(_) => "b h",
            BlockKind::Rule => "b r",
            _ => "b",
        };
        _ = write!(out, "<div class=\"{class}{}\" style=\"padding-left:{left}px", if block.mono { " m" } else { "" });
        if let BlockKind::Heading(level) = block.kind {
            _ = write!(out, ";font-size:{}px", if level <= 1 { 22 } else if level == 2 { 19 } else { 16 });
        }
        out.push_str("\">");
        for level in 0..u32::from(block.quote) {
            _ = write!(out, "<span class=\"q\" style=\"left:{}px\"></span>", 20 + u32::from(block.indent) * 18 + level * 14 + 2);
        }
        match block.kind {
            BlockKind::Paragraph | BlockKind::Heading(_) => {
                out.push_str(&html::styled_to_html(&readable_colors(&block.markdown, dark)));
            }
            BlockKind::Rule => out.push_str("<hr>"),
            BlockKind::Image => {
                let row: u32 = block.images.iter().map(|image| waiting_size(image.width, image.height).0 as u32 + GAP as u32).sum();
                _ = write!(out, "<div class=\"row\" style=\"width:{}px\">", row.saturating_sub(GAP as u32));
                for image in &block.images {
                    picture(&mut out, image, remote);
                }
                out.push_str("</div>");
            }
        }
        out.push_str("</div>");
    }
    out.push_str("</body></html>");
    out
}

/// A picture of a row, shrinking with the others when the row is too wide.
fn picture(out: &mut String, image: &ImageRef, remote: bool) {
    let (width, height) = waiting_size(image.width, image.height);
    if let Some(link) = &image.link {
        _ = write!(out, "<a href=\"{}\"", html::escape_html(link));
    } else {
        out.push_str("<span");
    }
    _ = write!(out, " style=\"flex:0 1 {width}px\">");
    let blocked = !remote && (image.src.starts_with("http://") || image.src.starts_with("https://"));
    if blocked {
        let alt = if image.alt.is_empty() { "Picture" } else { image.alt.as_str() };
        _ = write!(out, "<div class=\"alt\" style=\"width:{width}px;max-width:100%;height:{height}px\">{}</div>", html::escape_html(alt));
    } else {
        _ = write!(out, "<img src=\"{}\" alt=\"{}\"", html::escape_html(&image.src), html::escape_html(&image.alt));
        match (image.width, image.height) {
            (Some(w), Some(h)) => _ = write!(out, " style=\"width:{w}px;aspect-ratio:{w}/{h};height:auto\""),
            (Some(w), None) => _ = write!(out, " style=\"width:{w}px;height:auto\""),
            (None, Some(h)) => _ = write!(out, " style=\"height:{h}px;width:auto\""),
            (None, None) => {}
        }
        out.push('>');
    }
    out.push_str(if image.link.is_some() { "</a>" } else { "</span>" });
}

/// The space a picture takes before it arrives: the sender's size, or a
/// line for its description.
fn waiting_size(width: Option<u32>, height: Option<u32>) -> (f32, f32) {
    match (width, height) {
        (Some(w), Some(h)) => (w as f32, h as f32),
        (Some(w), None) => (w as f32, WAITING_HEIGHT),
        (None, Some(h)) => (WAITING_WIDTH, h as f32),
        (None, None) => (WAITING_WIDTH, WAITING_HEIGHT),
    }
}

/// The size to show an arrived picture at: the sender's width (or height)
/// with the picture's own proportions, else its own size.
pub fn shown_size(declared: (Option<u32>, Option<u32>), natural: (f32, f32)) -> (f32, f32) {
    let (natural_width, natural_height) = (natural.0.max(1.0), natural.1.max(1.0));
    match declared {
        (Some(w), _) => (w as f32, w as f32 * natural_height / natural_width),
        (None, Some(h)) => (h as f32 * natural_width / natural_height, h as f32),
        (None, None) => (natural_width, natural_height),
    }
}

/// Lays the pictures out in a row and stores them in the block.
pub fn set_pictures(block: &mut BodyBlock, mut pictures: Vec<Picture>) {
    let mut x = 0.0;
    let mut height: f32 = 0.0;
    for picture in &mut pictures {
        picture.x = x;
        x += picture.width + GAP;
        height = height.max(picture.height);
    }
    block.row_width = (x - GAP).max(0.0);
    block.row_height = height;
    block.pictures = ModelRc::new(VecModel::from(pictures));
}

/// Mixes the sender's text colours towards white on a dark theme, or black
/// on a light one, until they stand out from the background.
fn readable_colors(markdown: &str, dark: bool) -> String {
    const OPEN: &str = "<font color=\"#";
    if !markdown.contains(OPEN) {
        return markdown.to_owned();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(at) = rest.find(OPEN) {
        let start = at + OPEN.len();
        out.push_str(&rest[..start]);
        let hex = rest.get(start..start + 6).unwrap_or_default();
        match u32::from_str_radix(hex, 16) {
            Ok(rgb) if hex.len() == 6 => {
                out.push_str(&format!("{:06x}", contrast(rgb, dark)));
                rest = &rest[start + 6..];
            }
            _ => rest = &rest[start..],
        }
    }
    out.push_str(rest);
    out
}

fn contrast(rgb: u32, dark: bool) -> u32 {
    let channels = [(rgb >> 16) as f32, ((rgb >> 8) & 0xff) as f32, (rgb & 0xff) as f32];
    let luminance = |c: &[f32; 3]| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0;
    let (target, toward) = if dark { (0.6, 255.0) } else { (0.45, 0.0) };
    let mut color = channels;
    for _ in 0..10 {
        let ok = if dark { luminance(&color) >= target } else { luminance(&color) <= target };
        if ok {
            break;
        }
        color = color.map(|c| c + (toward - c) * 0.2);
    }
    let [r, g, b] = color.map(|c| c.round().clamp(0.0, 255.0) as u32);
    (r << 16) | (g << 8) | b
}

/// The paragraph's markdown, or its plain text should Slint refuse it.
fn styled(markdown: &str, plain: &str) -> StyledText {
    StyledText::from_markdown(markdown).unwrap_or_else(|error| {
        log::debug!("plain text for a paragraph: {error}");
        StyledText::from_plain_text(plain)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_stand_out_from_the_theme() {
        let dark_red = "<font color=\"#7a1010\">x</font>";
        let on_dark = readable_colors(dark_red, true);
        let hex = u32::from_str_radix(&on_dark[14..20], 16).unwrap();
        let lum = |rgb: u32| (0.2126 * (rgb >> 16) as f32 + 0.7152 * ((rgb >> 8) & 0xff) as f32 + 0.0722 * (rgb & 0xff) as f32) / 255.0;
        assert!(lum(hex) >= 0.6, "{on_dark}");
        assert!((hex >> 16) > ((hex >> 8) & 0xff), "still reddish: {on_dark}");
        assert_eq!(readable_colors(dark_red, false), dark_red, "already readable on light");
        let yellow = "<font color=\"#ffe800\">x</font>";
        assert!(lum(u32::from_str_radix(&readable_colors(yellow, false)[14..20], 16).unwrap()) <= 0.45);
    }

    fn simplified(html: &str, dark: bool, remote: bool) -> crate::page::Page {
        let rendered = lookout_core::html::render(html);
        let colours = colours(dark);
        let hex = u32::from_str_radix(&colours.surface[1..], 16).unwrap();
        let paper = crate::page::Paper {
            background: ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8),
            dark,
            selection: Some(colours.selection),
        };
        let resources = crate::page::Resources::new(crate::engine::Engine::idle(), String::new(), String::new(), false, || {});
        let mut page = crate::page::Page::new(&page(&rendered, dark, remote), resources, paper);
        page.view(400.0, 300.0, 1.0);
        page
    }

    #[test]
    fn the_simplified_view_can_be_selected() {
        let mut page = simplified(
            "<p>Hello <b>there</b> <a href=\"https://x.test\">link</a></p><blockquote><p>Quoted line</p></blockquote>",
            true,
            false,
        );
        let painted = page.paint(0.0, 0.0).unwrap().picture.to_rgba8().unwrap();
        let corner = painted.as_slice()[0];
        assert_eq!((corner.r, corner.g, corner.b), (0x24, 0x24, 0x27), "the theme's surface");
        // The middle of the first line.
        let y = 16.0;
        assert!(page.text_at(30.0, y));
        page.select_from(21.0, y);
        page.select_to(390.0, y + 28.0);
        assert_eq!(page.selected_text().as_deref(), Some("Hello there link\nQuoted line"));
        assert_eq!(page.link_at(page_x_of_link(&page, y), y).as_deref(), Some("https://x.test"));
        let selected = page.paint(0.0, 0.0).unwrap().picture.to_rgba8().unwrap();
        let at = |x: usize, y: usize| selected.as_slice()[y * 400 + x];
        let (inside, outside) = (at(22, y as usize), at(22, 290));
        assert!(inside.b > inside.r + 20, "tinted with the accent: {inside:?}");
        assert_eq!((outside.r, outside.g, outside.b), (0x24, 0x24, 0x27));
        // Not Blitz's own pale blue.
        assert!(!selected.as_slice().iter().any(|p| (p.r, p.g, p.b) == (180, 213, 255)));
        assert!(page.selected_text().is_some(), "the selection survives painting");
    }

    fn page_x_of_link(page: &crate::page::Page, y: f32) -> f32 {
        (0..400).map(|x| x as f32).find(|&x| page.link_at(x, y).is_some()).unwrap()
    }

    #[test]
    fn pictures_from_the_internet_wait_to_be_allowed() {
        let rendered = lookout_core::html::render(
            "<img src=\"https://x.test/a.png\" alt=\"Logo\" width=\"120\" height=\"40\"><img src=\"cid:b\" width=\"10\" height=\"10\">",
        );
        let blocked = page(&rendered, false, false);
        assert!(blocked.contains(">Logo</div>") && !blocked.contains("https://x.test/a.png"), "{blocked}");
        assert!(blocked.contains("<img src=\"cid:b\""), "{blocked}");
        assert!(page(&rendered, false, true).contains("<img src=\"https://x.test/a.png\""));
    }

    /// Everything the converter writes must be markdown Slint accepts.
    #[test]
    fn converted_html_is_accepted_by_slint() {
        let samples = [
            "<p>price_list_2024.pdf *not bold* # 1. [x] <b>bold</b> <i>it</i> <s>gone</s></p>",
            r#"<p><a href="https://example.com/a?b=c&d=e">link <b>bold</b></a> after</p>"#,
            r#"<p><span style="color:#cc0000">red <u>under</u></span> & &amp; &lt;tag&gt; `tick` \back</p>"#,
            "<p>Price:<b>$5</b> and <b>(note)</b> x<b>(y)</b>z</p>",
            "<p>one<br>two<br>- dash<br>+ plus<br>=== equals<br>1) paren</p>",
            "<ol><li>first</li><li>second <b>bold</b></li></ol>",
            "<blockquote><p>&gt; quoted</p></blockquote>",
            "<pre>  code *here*\n    indented</pre>",
            "<p>see https://example.com/x_(y). and https://</p>",
            "<p><b><i>both</i></b> <b>a</b><i>b</i> <font color=\"#00f\">blue</font></p>",
            "<table><tr><td><b>A</b></td><td><i>B</i></td></tr></table>",
            "<p>***</p><p>---</p><p>| a | b |</p><p>![img](x)</p>",
        ];
        for html in samples {
            let rendered = lookout_core::html::render(html);
            for block in &rendered.blocks {
                let block = &lookout_core::html::Block { markdown: readable_colors(&block.markdown, true), ..block.clone() };
                if matches!(block.kind, BlockKind::Paragraph | BlockKind::Heading(_)) {
                    assert!(
                        StyledText::from_markdown(&block.markdown).is_ok(),
                        "{html:?} gave {:?}: {:?}",
                        block.markdown,
                        StyledText::from_markdown(&block.markdown).err().map(|e| e.to_string())
                    );
                }
            }
        }
    }
}
