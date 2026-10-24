//! A message body as the reading pane's blocks.

use lookout_core::html::{BlockKind, Rendered};
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
