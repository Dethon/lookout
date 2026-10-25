//! Turns a message's HTML into blocks the interface draws natively.
//!
//! Text becomes the markdown dialect of Slint's `StyledText`: bold, italic,
//! strikethrough, links, `<u>` and `<font color>`. Everything that dialect
//! can't hold — headings, quotes, rules, pictures — becomes a block of its
//! own. Tables are read row by row. Scripts, styles and hidden parts are
//! dropped, and markdown in the text is escaped so it shows as written.

use html5gum::{DefaultEmitter, Token, Tokenizer};

// Past this many blocks the rest is left to "Show original".
const MAX_BLOCKS: usize = 4000;
// Pictures this small are tracking pixels or spacers.
const MIN_IMAGE_SIDE: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 1 to 6.
    Heading(u8),
    Rule,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// `StyledText` markdown for paragraphs and headings.
    pub markdown: String,
    /// The text without styling.
    pub plain: String,
    /// Nesting of lists, for the left margin.
    pub indent: u8,
    /// Nesting of quotes, each drawn as a bar.
    pub quote: u8,
    /// Monospaced, as in `<pre>`.
    pub mono: bool,
    /// The pictures of an image block, side by side: those in one table row,
    /// or next to each other in the text.
    pub images: Vec<ImageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// `http(s)://…`, `cid:…` or `data:…`.
    pub src: String,
    pub alt: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Where clicking the picture goes.
    pub link: Option<String>,
}

#[derive(Debug, Default)]
pub struct Rendered {
    pub blocks: Vec<Block>,
    /// The whole text, for searching.
    pub text: String,
    pub has_remote_images: bool,
    /// Blocks were left out past the limit.
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    color: Option<String>,
    link: Option<String>,
}

impl Style {
    fn has_emphasis(&self) -> bool {
        self.bold || self.italic || self.strike
    }

    fn without_emphasis(&self) -> Style {
        Style { bold: false, italic: false, strike: false, ..self.clone() }
    }
}

#[derive(Debug, Clone)]
struct Run {
    text: String,
    style: Style,
}

/// What an open element changed, to undo when it closes.
#[derive(Debug)]
struct Open {
    name: String,
    style: Style,
    hidden: bool,
    list: Option<List>,
    quote: bool,
    pre: bool,
    heading: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
enum List {
    Bullets,
    Numbers(u32),
}

struct Converter {
    out: Rendered,
    runs: Vec<Run>,
    /// The paragraph so far ends in a space (or is empty), so the next
    /// space is collapsed.
    space_pending: bool,
    stack: Vec<Open>,
    style: Style,
    hidden: u32,
    lists: Vec<List>,
    quote: u8,
    pre: u32,
    heading: Option<u8>,
    tables: u32,
    /// Counts table rows, and block boundaries outside tables: pictures
    /// with the same count share a block.
    row: u32,
    /// The count the last image block was made for, while nothing else has
    /// been added after it.
    image_row: Option<u32>,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
// Elements whose content is never shown.
const SKIPPED: &[&str] = &["head", "style", "script", "title", "template", "xml", "object", "svg", "select"];
const BLOCK: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "dd", "div", "dl", "dt", "fieldset",
    "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "li",
    "main", "nav", "ol", "p", "pre", "section", "table", "tbody", "thead", "tfoot", "tr", "ul",
    "caption", "body", "html",
];

pub fn render(html: &str) -> Rendered {
    let mut emitter = DefaultEmitter::default();
    emitter.naively_switch_states(true);
    let tokenizer = Tokenizer::new_with_emitter(html, emitter);
    let mut converter = Converter {
        out: Rendered::default(),
        runs: Vec::new(),
        space_pending: true,
        stack: Vec::new(),
        style: Style::default(),
        hidden: 0,
        lists: Vec::new(),
        quote: 0,
        pre: 0,
        heading: None,
        tables: 0,
        row: 0,
        image_row: None,
    };
    for token in tokenizer {
        let Ok(token) = token;
        if converter.out.blocks.len() >= MAX_BLOCKS {
            converter.out.truncated = true;
            break;
        }
        match token {
            Token::StartTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                let attribute = |key: &str| {
                    tag.attributes
                        .get(key.as_bytes())
                        .map(|value| String::from_utf8_lossy(value).into_owned())
                };
                converter.start(&name, &attribute, tag.self_closing);
            }
            Token::EndTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                converter.end(&name);
            }
            Token::String(text)
                if converter.hidden == 0 => {
                    converter.text(&String::from_utf8_lossy(&text));
                }
            _ => {}
        }
    }
    converter.flush();
    let mut out = converter.out;
    out.text = out
        .blocks
        .iter()
        .filter(|block| !block.plain.is_empty())
        .map(|block| block.plain.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    out
}

/// Plain text as blocks, for messages without HTML.
pub fn render_text(text: &str) -> Rendered {
    let mut html = String::with_capacity(text.len() + 32);
    html.push_str("<div>");
    for line in text.lines() {
        html.push_str(&escape_html(line));
        html.push_str("<br>");
    }
    html.push_str("</div>");
    render(&html)
}

pub fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// HTML for a message written in the composer: markdown, with each line
/// break kept as typed. Anything typed as HTML is shown as written.
pub fn markdown_to_html(text: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;");
    let events = Parser::new_ext(&escaped, Options::ENABLE_STRIKETHROUGH).map(|event| match event {
        Event::SoftBreak => Event::HardBreak,
        Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
        other => other,
    });
    let mut html = String::from("<div style=\"font-family:Segoe UI,sans-serif;font-size:11pt\">");
    pulldown_cmark::html::push_html(&mut html, events);
    html.push_str("</div>");
    html
}

/// HTML for a paragraph's markdown, as `render` writes it: inline only,
/// each line break kept, and `<font color>` as a coloured span. Nothing
/// else in it is read as HTML.
pub fn styled_to_html(markdown: &str) -> String {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};
    let mut paragraphs = 0;
    let events = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH).filter_map(|event| match event {
        Event::Start(Tag::Paragraph) => {
            paragraphs += 1;
            // An empty line in the text, which markdown takes as the start
            // of another paragraph.
            (paragraphs > 1).then(|| Event::Html("<br><br>".into()))
        }
        Event::End(TagEnd::Paragraph) => None,
        Event::SoftBreak => Some(Event::HardBreak),
        // Spaces as written, as between table cells, rather than collapsed.
        Event::Text(text) if text.contains("  ") => {
            let mut previous = ' ';
            let kept: String = text
                .chars()
                .map(|c| {
                    let kept = if c == ' ' && previous == ' ' { '\u{a0}' } else { c };
                    previous = c;
                    kept
                })
                .collect();
            Some(Event::Text(kept.into()))
        }
        Event::Html(tag) | Event::InlineHtml(tag) => Some(match tag.as_ref() {
            "<u>" | "</u>" => Event::InlineHtml(tag),
            "</font>" => Event::InlineHtml("</span>".into()),
            _ => match tag.strip_prefix("<font color=\"#").and_then(|rest| rest.strip_suffix("\">")) {
                Some(hex) if hex.chars().all(|c| c.is_ascii_hexdigit()) => {
                    Event::InlineHtml(CowStr::from(format!("<span style=\"color:#{hex}\">")))
                }
                _ => Event::Text(tag),
            },
        }),
        other => Some(other),
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

/// HTML for text the user typed, each line kept.
pub fn text_to_html(text: &str) -> String {
    let mut html = String::from("<div style=\"font-family:Segoe UI,sans-serif;font-size:11pt\">");
    for line in text.lines() {
        if line.is_empty() {
            html.push_str("<div><br></div>");
        } else {
            html.push_str("<div>");
            html.push_str(&escape_html(line).replace("  ", " &nbsp;"));
            html.push_str("</div>");
        }
    }
    html.push_str("</div>");
    html
}

impl Converter {
    fn start(&mut self, name: &str, attribute: &dyn Fn(&str) -> Option<String>, self_closing: bool) {
        let inline_style = attribute("style").unwrap_or_default().to_ascii_lowercase();
        let hidden = SKIPPED.contains(&name)
            || hides(&inline_style)
            || attribute("hidden").is_some()
            || (name == "input" || name == "button");

        if self.hidden > 0 {
            if !VOID.contains(&name) && !self_closing {
                self.push_open(name, true);
            }
            return;
        }

        match name {
            "br" => {
                self.line_break();
                return;
            }
            "hr" => {
                self.flush();
                self.block(BlockKind::Rule, String::new(), String::new(), Vec::new());
                return;
            }
            "img" => {
                self.image(attribute);
                return;
            }
            "td" | "th" => self.separate(),
            _ => {}
        }
        if BLOCK.contains(&name) {
            self.flush();
            if self.tables == 0 || name == "tr" || name == "table" {
                self.row += 1;
            }
        }
        if name == "table" && !self_closing {
            self.tables += 1;
        }
        if VOID.contains(&name) || self_closing {
            return;
        }

        let mut style = self.style.clone();
        match name {
            "b" | "strong" | "th" => style.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" => style.italic = true,
            "u" | "ins" => style.underline = true,
            "s" | "strike" | "del" => style.strike = true,
            "a" => {
                style.link = attribute("href").and_then(|href| safe_link(&href)).or(style.link);
            }
            "font" => {
                if let Some(color) = attribute("color").and_then(|color| parse_color(&color)) {
                    style.color = Some(color);
                }
            }
            _ => {}
        }
        if let Some(weight) = css_value(&inline_style, "font-weight") {
            style.bold = weight == "bold" || weight == "bolder" || weight.parse::<u32>().is_ok_and(|w| w >= 600);
        }
        if css_value(&inline_style, "font-style").is_some_and(|value| value == "italic") {
            style.italic = true;
        }
        if css_value(&inline_style, "text-decoration").is_some_and(|value| value.contains("underline")) {
            style.underline = true;
        }
        if css_value(&inline_style, "text-decoration").is_some_and(|value| value.contains("line-through")) {
            style.strike = true;
        }
        if let Some(color) = css_value(&inline_style, "color").and_then(parse_color) {
            style.color = Some(color);
        }

        let previous = std::mem::replace(&mut self.style, style);
        let list = match name {
            "ul" => Some(List::Bullets),
            "ol" => Some(List::Numbers(attribute("start").and_then(|s| s.parse().ok()).unwrap_or(1))),
            _ => None,
        };
        if let Some(list) = list {
            self.lists.push(list);
        }
        if name == "li" {
            self.list_marker();
        }
        let quote = name == "blockquote";
        if quote {
            self.quote = self.quote.saturating_add(1);
        }
        let pre = name == "pre";
        if pre {
            self.pre += 1;
        }
        let heading = match name.as_bytes() {
            [b'h', level @ b'1'..=b'6'] => Some(level - b'0'),
            _ => None,
        };
        let previous_heading = self.heading;
        if heading.is_some() {
            self.heading = heading;
        }
        if hidden {
            self.hidden += 1;
        }
        self.stack.push(Open {
            name: name.to_owned(),
            style: previous,
            hidden,
            list: list.map(|_| List::Bullets),
            quote,
            pre,
            heading: if heading.is_some() { Some(previous_heading.unwrap_or(0)) } else { None },
        });
    }

    fn push_open(&mut self, name: &str, hidden: bool) {
        if hidden {
            self.hidden += 1;
        }
        self.stack.push(Open {
            name: name.to_owned(),
            style: self.style.clone(),
            hidden,
            list: None,
            quote: false,
            pre: false,
            heading: None,
        });
    }

    fn end(&mut self, name: &str) {
        // Tags closed out of order: close up to the matching one, if any.
        let Some(position) = self.stack.iter().rposition(|open| open.name == name) else {
            if name == "p" || name == "br" {
                self.flush();
            }
            return;
        };
        while self.stack.len() > position {
            let open = self.stack.pop().expect("the stack has the element");
            self.close(open);
        }
    }

    fn close(&mut self, open: Open) {
        if open.hidden {
            self.hidden -= 1;
        }
        let shown = self.hidden == 0 && !open.hidden;
        if open.name == "table" {
            self.tables = self.tables.saturating_sub(1);
        }
        if BLOCK.contains(&open.name.as_str()) && (self.tables == 0 || open.name == "tr" || open.name == "table") {
            self.row += 1;
        }
        if shown && (BLOCK.contains(&open.name.as_str()) || open.name == "td" || open.name == "th") {
            if open.name == "td" || open.name == "th" {
                self.separate();
            } else {
                self.flush();
            }
        }
        self.style = open.style;
        if open.list.is_some() {
            self.lists.pop();
        }
        if open.quote {
            self.quote = self.quote.saturating_sub(1);
        }
        if open.pre {
            self.pre -= 1;
        }
        if let Some(previous) = open.heading {
            self.heading = (previous != 0).then_some(previous);
        }
    }

    fn text(&mut self, text: &str) {
        if self.pre > 0 {
            let mut lines = text.split('\n');
            if let Some(first) = lines.next() {
                self.push_text(first);
            }
            for line in lines {
                self.line_break();
                self.push_text(line);
            }
            return;
        }
        let mut collapsed = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                if !self.space_pending && !collapsed.ends_with(' ') {
                    collapsed.push(' ');
                }
            } else {
                collapsed.push(c);
                self.space_pending = false;
            }
        }
        if collapsed.ends_with(' ') {
            self.space_pending = true;
        }
        if !collapsed.is_empty() {
            self.append(collapsed);
        }
    }

    /// Text inside `<pre>`: spaces kept, as no-break spaces markdown leaves alone.
    fn push_text(&mut self, text: &str) {
        let kept: String = text.chars().map(|c| if c == ' ' || c == '\t' { '\u{a0}' } else { c }).collect();
        if !kept.is_empty() {
            self.space_pending = false;
            self.append(kept);
        }
    }

    fn append(&mut self, text: String) {
        match self.runs.last_mut() {
            Some(last) if last.style == self.style => last.text.push_str(&text),
            _ => self.runs.push(Run { text, style: self.style.clone() }),
        }
    }

    fn line_break(&mut self) {
        if let Some(last) = self.runs.last_mut()
            && last.text.ends_with(' ')
        {
            last.text.pop();
        }
        self.runs.push(Run { text: "\n".into(), style: Style::default() });
        self.space_pending = true;
    }

    /// Between table cells: a gap, unless the cell started a new paragraph.
    fn separate(&mut self) {
        if !self.runs.is_empty() && !self.space_pending {
            self.append("   ".into());
            self.space_pending = true;
        }
    }

    fn list_marker(&mut self) {
        let marker = match self.lists.last_mut() {
            Some(List::Numbers(next)) => {
                let marker = format!("{next}. ");
                *next += 1;
                marker
            }
            _ => "• ".to_owned(),
        };
        self.runs.push(Run { text: marker, style: Style::default() });
        self.space_pending = true;
    }

    fn image(&mut self, attribute: &dyn Fn(&str) -> Option<String>) {
        let Some(src) = attribute("src").map(|src| src.trim().to_owned()).filter(|src| !src.is_empty()) else {
            return;
        };
        let dimension = |key: &str| {
            attribute(key).and_then(|value| value.trim().trim_end_matches("px").parse::<f32>().ok()).map(|v| v as u32)
        };
        let style = attribute("style").unwrap_or_default().to_ascii_lowercase();
        let width = dimension("width").or_else(|| css_value(&style, "width").and_then(|v| v.trim_end_matches("px").parse::<f32>().ok()).map(|v| v as u32));
        let height = dimension("height").or_else(|| css_value(&style, "height").and_then(|v| v.trim_end_matches("px").parse::<f32>().ok()).map(|v| v as u32));
        if width.is_some_and(|w| w < MIN_IMAGE_SIDE) || height.is_some_and(|h| h < MIN_IMAGE_SIDE) || hides(&style) {
            return;
        }
        let lower = src.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("cid:") || lower.starts_with("data:image/")) {
            return;
        }
        if lower.starts_with("http") {
            self.out.has_remote_images = true;
        }
        self.flush();
        let alt = attribute("alt").unwrap_or_default().trim().to_owned();
        let image = ImageRef { src, alt: alt.clone(), width, height, link: self.style.link.clone() };
        if self.image_row == Some(self.row)
            && let Some(last) = self.out.blocks.last_mut()
            && last.kind == BlockKind::Image
        {
            if !alt.is_empty() {
                if !last.plain.is_empty() {
                    last.plain.push(' ');
                }
                last.plain.push_str(&alt);
            }
            last.images.push(image);
            return;
        }
        self.block(BlockKind::Image, String::new(), alt, vec![image]);
        self.image_row = Some(self.row);
    }

    fn block(&mut self, kind: BlockKind, markdown: String, plain: String, images: Vec<ImageRef>) {
        self.image_row = None;
        self.out.blocks.push(Block {
            kind,
            markdown,
            plain,
            indent: self.lists.len().min(8) as u8,
            quote: self.quote,
            mono: self.pre > 0,
            images,
        });
    }

    /// Ends the paragraph being built.
    fn flush(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        self.space_pending = true;
        let runs = tidy(runs);
        if runs.is_empty() {
            return;
        }
        let plain: String = runs.iter().map(|run| run.text.as_str()).collect();
        if plain.trim().is_empty() {
            return;
        }
        let markdown = to_markdown(&runs);
        let kind = match self.heading {
            Some(level) => BlockKind::Heading(level),
            None => BlockKind::Paragraph,
        };
        self.block(kind, markdown, plain, Vec::new());
    }
}

/// Trims the paragraph, splits off the spaces around emphasis (markdown
/// won't open or close emphasis next to a space), finds bare links, and
/// merges runs that ended up alike.
fn tidy(runs: Vec<Run>) -> Vec<Run> {
    let mut split = Vec::with_capacity(runs.len());
    for run in runs {
        if !run.style.has_emphasis() || run.text == "\n" {
            split.extend(link_bare_urls(run));
            continue;
        }
        let core = run.text.trim_matches(is_space);
        let start = run.text.len() - run.text.trim_start_matches(is_space).len();
        let lead = &run.text[..start];
        let trail = &run.text[start + core.len()..];
        if !lead.is_empty() {
            split.push(Run { text: lead.to_owned(), style: run.style.without_emphasis() });
        }
        if !core.is_empty() {
            split.extend(link_bare_urls(Run { text: core.to_owned(), style: run.style.clone() }));
        }
        if !trail.is_empty() {
            split.push(Run { text: trail.to_owned(), style: run.style.without_emphasis() });
        }
    }

    let mut merged: Vec<Run> = Vec::with_capacity(split.len());
    for run in split {
        match merged.last_mut() {
            Some(last) if last.style == run.style && run.text != "\n" && last.text != "\n" => last.text.push_str(&run.text),
            _ => merged.push(run),
        }
    }
    // Spaces at the ends of the paragraph and of its lines.
    while merged.first().is_some_and(|run| run.text.trim_matches(is_space).is_empty()) {
        merged.remove(0);
    }
    while merged.last().is_some_and(|run| run.text.trim_matches(is_space).is_empty()) {
        merged.pop();
    }
    if let Some(first) = merged.first_mut() {
        first.text = first.text.trim_start_matches(is_space).to_owned();
    }
    if let Some(last) = merged.last_mut() {
        last.text = last.text.trim_end_matches(is_space).to_owned();
    }
    merged
}

fn is_space(c: char) -> bool {
    c == ' '
}

/// Splits `http://…` and `https://…` in unlinked text into links.
fn link_bare_urls(run: Run) -> Vec<Run> {
    if run.style.link.is_some() || !(run.text.contains("http://") || run.text.contains("https://")) {
        return vec![run];
    }
    let mut runs = Vec::new();
    let mut rest = run.text.as_str();
    while let Some(start) = rest.find("https://").into_iter().chain(rest.find("http://")).min() {
        let tail = &rest[start..];
        let mut end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\u{a0}')).unwrap_or(tail.len());
        while end > 0 && tail[..end].ends_with(['.', ',', ';', ':', ')', '!', '?', '\'']) {
            end -= 1;
        }
        let url = &tail[..end];
        if url.len() <= "https://".len() {
            runs.push(Run { text: rest[..start + end.max(1)].to_owned(), style: run.style.clone() });
            rest = &rest[start + end.max(1)..];
            continue;
        }
        if start > 0 {
            runs.push(Run { text: rest[..start].to_owned(), style: run.style.clone() });
        }
        let mut style = run.style.clone();
        style.link = safe_link(url);
        runs.push(Run { text: url.to_owned(), style });
        rest = &tail[end..];
    }
    if !rest.is_empty() {
        runs.push(Run { text: rest.to_owned(), style: run.style });
    }
    runs
}

fn to_markdown(runs: &[Run]) -> String {
    let mut markdown = String::new();
    let mut link: Option<&str> = None;
    for (index, run) in runs.iter().enumerate() {
        if run.text == "\n" {
            if let Some(url) = link.take() {
                close_link(&mut markdown, url);
            }
            markdown.push('\n');
            continue;
        }
        let run_link = run.style.link.as_deref();
        if run_link != link {
            if let Some(url) = link {
                close_link(&mut markdown, url);
            }
            if run_link.is_some() {
                markdown.push('[');
            }
            link = run_link;
        }

        let previous = index.checked_sub(1).and_then(|i| runs[i].text.chars().last());
        let next = runs.get(index + 1).and_then(|run| run.text.chars().next());
        let touches_emphasis = index
            .checked_sub(1)
            .is_some_and(|i| runs[i].style.has_emphasis() && runs[i].text != "\n");
        let emphasis = run.style.has_emphasis()
            && !touches_emphasis
            && can_open(run.text.chars().next(), previous)
            && can_close(run.text.chars().last(), next);

        if let Some(color) = &run.style.color {
            markdown.push_str("<font color=\"");
            markdown.push_str(color);
            markdown.push_str("\">");
        }
        if run.style.underline {
            markdown.push_str("<u>");
        }
        let markers = if emphasis {
            let mut markers = String::new();
            if run.style.strike {
                markers.push_str("~~");
            }
            if run.style.bold {
                markers.push_str("**");
            }
            if run.style.italic {
                markers.push('*');
            }
            markers
        } else {
            String::new()
        };
        markdown.push_str(&markers);
        escape_markdown(&run.text, &mut markdown);
        markdown.extend(markers.chars().rev());
        if run.style.underline {
            markdown.push_str("</u>");
        }
        if run.style.color.is_some() {
            markdown.push_str("</font>");
        }
    }
    if let Some(url) = link {
        close_link(&mut markdown, url);
    }
    markdown
}

fn close_link(markdown: &mut String, url: &str) {
    markdown.push_str("](<");
    markdown.push_str(url);
    markdown.push_str(">)");
}

fn is_markdown_whitespace(c: char) -> bool {
    c.is_whitespace()
}

fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace())
}

/// Markdown opens emphasis before a punctuation mark only after a space or
/// another mark.
fn can_open(first: Option<char>, previous: Option<char>) -> bool {
    match (first, previous) {
        (None, _) => false,
        (Some(c), _) if is_markdown_whitespace(c) => false,
        (Some(c), Some(p)) if is_punctuation(c) => is_markdown_whitespace(p) || is_punctuation(p),
        _ => true,
    }
}

fn can_close(last: Option<char>, next: Option<char>) -> bool {
    match (last, next) {
        (None, _) => false,
        (Some(c), _) if is_markdown_whitespace(c) => false,
        (Some(c), Some(n)) if is_punctuation(c) => is_markdown_whitespace(n) || is_punctuation(n),
        _ => true,
    }
}

/// Backslash-escapes every ASCII punctuation mark, which CommonMark always
/// allows, so nothing in the text is read as markup.
fn escape_markdown(text: &str, out: &mut String) {
    for c in text.chars() {
        if c == '\n' {
            out.push('\n');
        } else if c.is_ascii_punctuation() {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }
}

/// Only links the browser can open safely: web pages and mail addresses.
fn safe_link(href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return None;
    }
    // Inside `<…>` a link can't hold spaces or angle brackets.
    let cleaned: String = href
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n' | '\r' | '\t'))
        .map(|c| if c == ' ' { "%20".to_owned() } else { c.to_string() })
        .collect();
    Some(cleaned)
}

fn hides(style: &str) -> bool {
    css_value(style, "display").is_some_and(|value| value.starts_with("none"))
        || css_value(style, "visibility").is_some_and(|value| value.starts_with("hidden"))
        || css_value(style, "max-height").is_some_and(|value| value.starts_with('0'))
            && css_value(style, "overflow").is_some_and(|value| value.starts_with("hidden"))
        || css_value(style, "font-size").is_some_and(|value| value.starts_with('0') && !value.starts_with("0."))
        || css_value(style, "mso-hide").is_some_and(|value| value.starts_with("all"))
}

/// The last value of `property` in an inline style, lowercased.
fn css_value<'a>(style: &'a str, property: &str) -> Option<&'a str> {
    style
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .filter(|(name, _)| name.trim() == property)
        .map(|(_, value)| value.trim().trim_end_matches("!important").trim())
        .next_back()
}

/// A colour as `#rrggbb`, if it is one text can sensibly take. Near-black
/// and near-white are left out: they are the sender's default text or
/// background colour, and would vanish against the reader's theme.
fn parse_color(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let (r, g, b) = if let Some(hex) = value.strip_prefix('#') {
        match hex.len() {
            3 => {
                let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|d| d * 17);
                (digit(0)?, digit(1)?, digit(2)?)
            }
            6 => {
                let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                (pair(0)?, pair(2)?, pair(4)?)
            }
            _ => return None,
        }
    } else if let Some(inner) = value.strip_prefix("rgb(").or_else(|| value.strip_prefix("rgba(")) {
        let mut parts = inner.trim_end_matches(')').split(',').map(|part| part.trim().parse::<f32>().ok());
        let mut next = || parts.next().flatten().map(|v| v.clamp(0.0, 255.0) as u8);
        (next()?, next()?, next()?)
    } else {
        match value.as_str() {
            "red" => (0xff, 0, 0),
            "green" => (0, 0x80, 0),
            "blue" => (0, 0, 0xff),
            "navy" => (0, 0, 0x80),
            "maroon" => (0x80, 0, 0),
            "purple" => (0x80, 0, 0x80),
            "orange" => (0xff, 0xa5, 0),
            "teal" => (0, 0x80, 0x80),
            "gray" | "grey" => (0x80, 0x80, 0x80),
            _ => return None,
        }
    };
    let luminance = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    if !(0.12..=0.88).contains(&luminance) {
        return None;
    }
    // Greys carry no meaning and clash with the theme's own text colour.
    let spread = r.max(g).max(b) - r.min(g).min(b);
    if spread < 24 {
        return None;
    }
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraphs(html: &str) -> Vec<String> {
        render(html)
            .blocks
            .into_iter()
            .filter(|block| block.kind == BlockKind::Paragraph)
            .map(|block| block.markdown)
            .collect()
    }

    #[test]
    fn markdown_in_text_is_escaped() {
        assert_eq!(paragraphs("<p>price_list_2024.pdf *not bold* # 1. [x]</p>"), [
            r"price\_list\_2024\.pdf \*not bold\* \# 1\. \[x\]"
        ]);
    }

    #[test]
    fn whitespace_collapses() {
        assert_eq!(paragraphs("<p>  a \n\n  b  </p>"), ["a b"]);
    }

    #[test]
    fn emphasis_keeps_its_spaces_outside() {
        assert_eq!(paragraphs("<p>a<b> bold </b>c</p>"), ["a **bold** c"]);
        assert_eq!(paragraphs("<p><i>it</i> and <s>gone</s></p>"), ["*it* and ~~gone~~"]);
    }

    #[test]
    fn emphasis_that_markdown_cant_express_is_dropped() {
        // Opening before a mark right after a letter would show the asterisks.
        assert_eq!(paragraphs("<p>Price<b>$5</b></p>"), [r"Price\$5"]);
        // After a mark it can.
        assert_eq!(paragraphs("<p>Price:<b>$5</b></p>"), [r"Price\:**\$5**"]);
        // Two kinds touching would merge their markers.
        assert_eq!(paragraphs("<p><b>a</b><i>b</i></p>"), ["**a**b"]);
    }

    #[test]
    fn links_are_kept_and_unsafe_ones_dropped() {
        assert_eq!(paragraphs(r#"<p><a href="https://example.com/a b">here</a></p>"#), [
            "[here](<https://example.com/a%20b>)"
        ]);
        assert_eq!(paragraphs(r#"<p><a href="javascript:alert(1)">x</a></p>"#), ["x"]);
    }

    #[test]
    fn bare_urls_become_links() {
        assert_eq!(paragraphs("<p>see https://example.com/x.</p>"), [
            r"see [https\:\/\/example\.com\/x](<https://example.com/x>)\."
        ]);
    }

    #[test]
    fn styles_scripts_and_hidden_parts_vanish() {
        let html = r#"<html><head><style>p { color: red }</style><title>T</title></head>
            <body><div style="display:none">preheader</div><script>var x = "<p>";</script><p>shown</p></body></html>"#;
        assert_eq!(paragraphs(html), ["shown"]);
    }

    #[test]
    fn office_conditional_comments_vanish() {
        assert_eq!(paragraphs("<!--[if mso]><p>outlook only</p><![endif]--><p>everyone</p>"), ["everyone"]);
    }

    #[test]
    fn colours_survive_but_black_and_white_dont() {
        assert_eq!(paragraphs(r#"<p><span style="color:#cc0000">red</span> <font color="black">black</font></p>"#), [
            r##"<font color="#cc0000">red</font> black"##
        ]);
    }

    #[test]
    fn breaks_lists_and_quotes() {
        let rendered = render("<p>one<br>two</p><ul><li>a</li><li>b</li></ul><blockquote><p>quoted</p></blockquote>");
        let summary: Vec<_> = rendered.blocks.iter().map(|b| (b.markdown.as_str(), b.indent, b.quote)).collect();
        assert_eq!(summary, [("one\ntwo", 0, 0), ("• a", 1, 0), ("• b", 1, 0), ("quoted", 0, 1)]);
    }

    #[test]
    fn numbered_lists_count() {
        assert_eq!(paragraphs("<ol start=3><li>x</li><li>y</li></ol>"), [r"3\. x", r"4\. y"]);
    }

    #[test]
    fn table_cells_sit_side_by_side() {
        assert_eq!(paragraphs("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"), ["a   b", "c"]);
    }

    #[test]
    fn headings_rules_and_images_are_blocks() {
        let rendered = render(
            r#"<h2>Title</h2><hr><a href="https://x.test"><img src="https://x.test/a.png" alt="A" width="100"></a>
               <img src="https://t.test/pixel.gif" width="1" height="1"><img src="cid:logo">"#,
        );
        let kinds: Vec<_> = rendered.blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(kinds, [BlockKind::Heading(2), BlockKind::Rule, BlockKind::Image]);
        assert_eq!(rendered.blocks[2].images.len(), 2, "the two pictures sit together");
        let image = &rendered.blocks[2].images[0];
        assert_eq!(image.link.as_deref(), Some("https://x.test"));
        assert_eq!(image.width, Some(100));
        assert!(rendered.has_remote_images);
    }

    #[test]
    fn pictures_in_a_table_row_share_a_block() {
        let rendered = render(
            r#"<table><tr><td><div><img src="cid:a"></div></td><td><div><img src="cid:b"></div></td></tr>
               <tr><td><img src="cid:c"></td></tr></table><p>text</p><img src="cid:d"><p>more</p><img src="cid:e">"#,
        );
        let rows: Vec<Vec<&str>> = rendered
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::Image)
            .map(|b| b.images.iter().map(|i| i.src.as_str()).collect())
            .collect();
        assert_eq!(rows, [vec!["cid:a", "cid:b"], vec!["cid:c"], vec!["cid:d"], vec!["cid:e"]]);
    }

    #[test]
    fn preformatted_text_keeps_its_spaces() {
        let rendered = render("<pre>a  b\n  c</pre>");
        assert_eq!(rendered.blocks[0].markdown, "a\u{a0}\u{a0}b\n\u{a0}\u{a0}c");
        assert!(rendered.blocks[0].mono);
    }

    #[test]
    fn misnested_tags_recover() {
        assert_eq!(paragraphs("<p><b>bold <i>both</b> after</i></p><p>next</p>"), ["**bold** ***both*** after", "next"]);
    }

    #[test]
    fn plain_text_is_searchable() {
        let rendered = render("<p>Hello <b>world</b></p><p>again</p>");
        assert_eq!(rendered.text, "Hello world\nagain");
    }

    #[test]
    fn composed_markdown_becomes_html() {
        let html = markdown_to_html("Hi **Ana**,\nsee *this*: <b>raw</b> & ~~that~~\n\n- one\n- two\n\n> quoted\n\n[site](https://x.test)");
        assert!(html.contains("Hi <strong>Ana</strong>,<br />"), "{html}");
        assert!(html.contains("<em>this</em>"), "{html}");
        assert!(html.contains("&lt;b&gt;raw&lt;/b&gt; &amp; <del>that</del>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");
        assert!(html.contains("<blockquote>"), "{html}");
        assert!(html.contains("<a href=\"https://x.test\">site</a>"), "{html}");
    }

    #[test]
    fn styled_text_becomes_html() {
        let rendered = render(
            "<p>Hi <b>Ana</b>, <a href=\"https://x.test/a?b=1&amp;c=2\">see <i>this</i></a><br>\
             <span style=\"color:#cc0000\">red <u>under</u></span> &lt;tag&gt; *not*<br><br>after</p>",
        );
        let html = styled_to_html(&rendered.blocks[0].markdown);
        assert_eq!(
            html,
            "Hi <strong>Ana</strong>, <a href=\"https://x.test/a?b=1&amp;c=2\">see <em>this</em></a><br />\n\
             <span style=\"color:#cc0000\">red </span><span style=\"color:#cc0000\"><u>under</u></span> &lt;tag&gt; *not*<br><br>after"
        );
        assert_eq!(styled_to_html("a   b"), "a \u{a0}\u{a0}b");
        assert_eq!(styled_to_html("<font color=\"#12\" onclick=\"x\">a</font>"), "&lt;font color=\"#12\" onclick=\"x\"&gt;a</span>");
    }

    #[test]
    fn typed_text_becomes_html() {
        assert_eq!(
            text_to_html("a <b>\n\nc"),
            "<div style=\"font-family:Segoe UI,sans-serif;font-size:11pt\"><div>a &lt;b&gt;</div><div><br></div><div>c</div></div>"
        );
    }
}
