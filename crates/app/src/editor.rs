//! The composer's formatting buttons: each edits the markdown being written
//! around the selection, and says what to select afterwards. Offsets are
//! byte offsets into the text, as the text input reports them.

#[derive(Debug, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub anchor: usize,
    pub cursor: usize,
}

const LINK_PLACEHOLDER: &str = "https://";

pub fn format(kind: &str, text: &str, anchor: usize, cursor: usize) -> Edit {
    let clamp = |offset: usize| {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let (start, end) = (clamp(anchor.min(cursor)), clamp(anchor.max(cursor)));
    match kind {
        "bold" => wrap(text, start, end, "**"),
        "italic" => wrap(text, start, end, "*"),
        "strike" => wrap(text, start, end, "~~"),
        "link" => link(text, start, end),
        "bullets" => prefix_lines(text, start, end, |_| "- ".into()),
        "numbers" => prefix_lines(text, start, end, |n| format!("{}. ", n + 1)),
        "quote" => prefix_lines(text, start, end, |_| "> ".into()),
        _ => Edit { text: text.to_owned(), anchor, cursor },
    }
}

/// Puts `marker` around the selection, or takes it away if it is there.
fn wrap(text: &str, start: usize, end: usize, marker: &str) -> Edit {
    // Spaces picked up at the ends of a selection stay outside the markers.
    let selected = &text[start..end];
    let start = start + (selected.len() - selected.trim_start().len());
    let end = end - (selected.len() - selected.trim_end().len()).min(end - start);
    let m = marker.len();
    let wrapped = start >= m && text[..start].ends_with(marker) && text[end..].starts_with(marker);
    if wrapped {
        let text = format!("{}{}{}", &text[..start - m], &text[start..end], &text[end + m..]);
        return Edit { text, anchor: start - m, cursor: end - m };
    }
    let text = format!("{}{marker}{}{marker}{}", &text[..start], &text[start..end], &text[end..]);
    Edit { text, anchor: start + m, cursor: end + m }
}

/// Makes the selection a link, leaving the address selected to type over.
fn link(text: &str, start: usize, end: usize) -> Edit {
    let label = &text[start..end];
    let looks_like_address = label.starts_with("http://") || label.starts_with("https://");
    let (shown, target) = if looks_like_address { (label, label) } else { (label, LINK_PLACEHOLDER) };
    let inserted = format!("[{shown}]({target})");
    let target_start = start + 1 + shown.len() + 2;
    let text = format!("{}{inserted}{}", &text[..start], &text[end..]);
    Edit { text, anchor: target_start, cursor: target_start + target.len() }
}

/// Starts every line of the selection with a prefix (a bullet, a number, a
/// quote mark), or removes them if all lines have one already.
fn prefix_lines(text: &str, start: usize, end: usize, prefix: impl Fn(usize) -> String) -> Edit {
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at);
    let lines: Vec<&str> = text[line_start..line_end].split('\n').collect();
    let has_prefix = |line: &str, n: usize| line.starts_with(&prefix(n));
    let remove = lines.iter().enumerate().all(|(n, line)| has_prefix(line, n));
    let changed: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(n, line)| {
            if remove {
                line[prefix(n).len()..].to_owned()
            } else {
                format!("{}{line}", prefix(n))
            }
        })
        .collect();
    let block = changed.join("\n");
    let text = format!("{}{block}{}", &text[..line_start], &text[line_end..]);
    // The cursor ends up after the edited lines, ready to type on.
    let after = line_start + block.len();
    Edit { text, anchor: after, cursor: after }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_wraps_and_unwraps() {
        let edit = format("bold", "make this strong", 5, 9);
        assert_eq!(edit, Edit { text: "make **this** strong".into(), anchor: 7, cursor: 11 });
        let undone = format("bold", &edit.text, edit.anchor, edit.cursor);
        assert_eq!(undone.text, "make this strong");
    }

    #[test]
    fn spaces_stay_outside() {
        assert_eq!(format("italic", "a word here", 1, 7).text, "a *word* here");
    }

    #[test]
    fn nothing_selected_leaves_the_cursor_between_markers() {
        let edit = format("bold", "ab", 1, 1);
        assert_eq!(edit, Edit { text: "a****b".into(), anchor: 3, cursor: 3 });
    }

    #[test]
    fn links_select_the_address() {
        let edit = format("link", "see docs now", 4, 8);
        assert_eq!(edit.text, "see [docs](https://) now");
        assert_eq!(&edit.text[edit.anchor..edit.cursor], "https://");
        assert_eq!(format("link", "https://x.test", 0, 14).text, "[https://x.test](https://x.test)");
    }

    #[test]
    fn lists_number_each_line_and_toggle_off() {
        let edit = format("numbers", "intro\nfirst\nsecond", 7, 15);
        assert_eq!(edit.text, "intro\n1. first\n2. second");
        assert_eq!(format("numbers", &edit.text, 7, 20).text, "intro\nfirst\nsecond");
        assert_eq!(format("bullets", "one", 0, 0).text, "- one");
        assert_eq!(format("quote", "x\ny", 0, 3).text, "> x\n> y");
    }

    #[test]
    fn offsets_inside_characters_are_safe() {
        // "é" is two bytes; an offset of 1 falls inside it and moves back.
        assert_eq!(format("bold", "é", 1, 2).text, "**é**");
    }
}
