//! the little HTML drive's pages need read: the text between two markers, an attribute of a tag,
//! and text with its tags and character references undone.

// unicode scalars need at most six hex digits or seven decimal digits.
const LONGEST_REFERENCE: usize = 10;

/// the text between `start` and the next `end` after it.
pub(super) fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

/// the value of `name="..."` in a tag, with its character references undone.
pub(super) fn attribute(tag: &str, name: &str) -> Option<String> {
    between(tag, &format!(" {name}=\""), "\"").map(unescape)
}

/// the words of a piece of HTML: its tags dropped and its character references undone.
pub(super) fn text(fragment: &str) -> String {
    let mut words = String::with_capacity(fragment.len());
    let mut rest = fragment;
    while let Some(open) = rest.find('<') {
        words.push_str(&rest[..open]);
        rest = rest[open..]
            .find('>')
            .map_or("", |close| &rest[open + close + 1..]);
    }
    words.push_str(rest);
    unescape(&words)
}

/// text with its character references, such as `&amp;` and `&#39;`, replaced by what they stand
/// for.
pub(super) fn unescape(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        plain.push_str(&rest[..at]);
        rest = &rest[at..];
        // an ASCII semicolon is a character boundary even when the lookahead ends inside UTF-8.
        let reference = rest
            .as_bytes()
            .iter()
            .take(LONGEST_REFERENCE)
            .position(|byte| *byte == b';')
            .and_then(|end| Some((character(&rest[1..end])?, end)));
        if let Some((c, end)) = reference {
            plain.push(c);
            rest = &rest[end + 1..];
        } else {
            plain.push('&');
            rest = &rest[1..];
        }
    }
    plain.push_str(rest);
    plain
}

/// the character a reference such as `amp` or `#39` names.
fn character(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_become_what_they_stand_for() {
        assert_eq!(
            unescape("a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &#x41;&#66;"),
            "a & b <c> \"d\" 'e' AB"
        );
        assert_eq!(
            unescape("fish &chips; & more &#xZZ; &#1114112;"),
            "fish &chips; & more &#xZZ; &#1114112;"
        );
    }

    #[test]
    fn attributes_are_read_by_their_whole_name() {
        let tag = r#"<input type="hidden" name="id" value="a&amp;b">"#;
        assert_eq!(attribute(tag, "name").as_deref(), Some("id"));
        assert_eq!(attribute(tag, "value").as_deref(), Some("a&b"));
        assert_eq!(attribute(tag, "id"), None);
        assert_eq!(attribute(r#"<div data-name="x">"#, "name"), None);
    }

    #[test]
    fn text_drops_tags() {
        assert_eq!(
            text(r#"try <a href="/x">again</a> &amp; wait"#),
            "try again & wait"
        );
        assert_eq!(between("<b>one</b><b>two</b>", "<b>", "</b>"), Some("one"));
        assert_eq!(between("<b>one", "<b>", "</b>"), None);
    }

    #[test]
    fn a_long_page_of_ampersands_has_bounded_work() {
        let mut page = "&".repeat(1024 * 1024 - 1);
        page.push(';');
        let expected = page.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = sender.send(unescape(&page));
        });
        let decoded = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("unescaping a long page did not finish: {error}"));
        assert_eq!(decoded, expected);
        worker.join().unwrap();
    }

    #[test]
    fn reference_lookahead_keeps_unicode_and_the_longest_scalars() {
        assert_eq!(
            unescape("&#x10FFFF;&#1114111;&#x1F600;"),
            "\u{10ffff}\u{10ffff}\u{1f600}"
        );
        assert_eq!(
            unescape(
                "&\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}; &\u{6f22}\u{6f22}\u{6f22}; &\u{1f600}\u{1f600}\u{1f600}; &amp;"
            ),
            "&\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}; &\u{6f22}\u{6f22}\u{6f22}; &\u{1f600}\u{1f600}\u{1f600}; &"
        );
    }
}
