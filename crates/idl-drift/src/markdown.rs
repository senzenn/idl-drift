//! Text for the `--markdown` reports of both CLIs.

/// `text` as literal Markdown text, for a table cell or a heading.
///
/// IDL names and messages are untrusted. A backslash before each ASCII punctuation
/// character makes it literal (the `CommonMark` spec allows this for each one), so no HTML,
/// link, emphasis or table break can start. A newline becomes a space, so a cell stays on
/// its row.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escape_stops_html_links_and_table_breaks() {
        let name = "a|b`c`<img src=x onerror=alert(1)>[x](https://e.x)*_\\\n# h";
        assert_eq!(
            escape(name),
            "a\\|b\\`c\\`\\<img src\\=x onerror\\=alert\\(1\\)\\>\\[x\\]\\(https\\:\\/\\/e\\.x\\)\\*\\_\\\\ \\# h"
        );
    }
}
