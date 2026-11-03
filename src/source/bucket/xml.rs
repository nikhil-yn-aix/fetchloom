//! a small XML reader for S3 answers: elements, text and the five named entities. it refuses
//! document type declarations, so no entity can expand, and bounds how deep elements nest.

use std::borrow::Cow;

/// the deepest nesting an answer may use. S3 listings nest three levels.
const DEEPEST: usize = 32;

/// one element: its name without a namespace prefix, its text and the elements inside it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Element {
    pub(super) name: String,
    pub(super) text: String,
    pub(super) children: Vec<Element>,
}

impl Element {
    pub(super) fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// the text of the first child called `name`.
    pub(super) fn text_of(&self, name: &str) -> Option<&str> {
        self.children
            .iter()
            .find(|child| child.name == name)
            .map(|child| child.text.as_str())
    }
}

/// reads a whole document into its root element.
pub(super) fn parse(text: &str) -> Result<Element, String> {
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    loop {
        let at = rest.find('<').unwrap_or(rest.len());
        let (before, after) = rest.split_at(at);
        if let Some(element) = open.last_mut() {
            element.text.push_str(&unescape(before)?);
        } else if !before.trim().is_empty() {
            return Err("text lies outside the root element".to_owned());
        }
        if after.is_empty() {
            break;
        }
        if let Some(inside) = after.strip_prefix("<?") {
            let end = inside.find("?>").ok_or("a declaration is not closed")?;
            rest = &inside[end + 2..];
            continue;
        }
        if let Some(inside) = after.strip_prefix("<!--") {
            let end = inside.find("-->").ok_or("a comment is not closed")?;
            rest = &inside[end + 3..];
            continue;
        }
        if after.starts_with("<!") {
            return Err("document types and CDATA sections are not accepted".to_owned());
        }
        let end = tag_end(after).ok_or("a tag is not closed")?;
        let tag = &after[1..end];
        rest = &after[end + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let name = local(name.trim_end())?;
            let element = open.pop().ok_or("a closing tag has no opening tag")?;
            if element.name != name {
                return Err(format!("<{}> is closed by </{name}>", element.name));
            }
            close(element, &mut open, &mut root)?;
            continue;
        }
        let (tag, empty) = match tag.strip_suffix('/') {
            Some(tag) => (tag, true),
            None => (tag, false),
        };
        let name = tag
            .split(|c: char| c.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        let name = local(name)?;
        if open.is_empty() && root.is_some() {
            return Err("the document has two root elements".to_owned());
        }
        if open.len() >= DEEPEST {
            return Err(format!("elements nest deeper than {DEEPEST} levels"));
        }
        let element = Element {
            name: name.to_owned(),
            ..Element::default()
        };
        if empty {
            close(element, &mut open, &mut root)?;
        } else {
            open.push(element);
        }
    }
    if let Some(element) = open.last() {
        return Err(format!("<{}> is not closed", element.name));
    }
    root.ok_or_else(|| "the document has no root element".to_owned())
}

/// files a finished element under its parent, or as the root.
fn close(element: Element, open: &mut [Element], root: &mut Option<Element>) -> Result<(), String> {
    match open.last_mut() {
        Some(parent) => parent.children.push(element),
        None if root.is_none() => *root = Some(element),
        None => return Err("the document has two root elements".to_owned()),
    }
    Ok(())
}

/// where the tag starting `text` ends, past any `>` inside a quoted attribute value.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (None, '>') => return Some(at),
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            _ => {}
        }
    }
    None
}

/// a tag's name without its namespace prefix, when it is a valid name.
fn local(name: &str) -> Result<&str, String> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    let local = name.rsplit(':').next().unwrap_or(name);
    if valid && !local.is_empty() {
        Ok(local)
    } else {
        Err(format!("<{name}> is not a valid tag"))
    }
}

/// text with its entity and character references replaced.
fn unescape(text: &str) -> Result<Cow<'_, str>, String> {
    if !text.contains('&') {
        return Ok(Cow::Borrowed(text));
    }
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        plain.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after
            .find(';')
            .filter(|end| *end <= 10)
            .ok_or("an & starts no reference")?;
        let name = &after[..end];
        let character = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = name
                    .strip_prefix('#')
                    .ok_or_else(|| format!("&{name}; is not a known entity"))?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => number.parse(),
                }
                .map_err(|_| format!("&{name}; is not a character reference"))?;
                char::from_u32(code)
                    .filter(|c| *c != '\0')
                    .ok_or_else(|| format!("&{name}; names no character"))?
            }
        };
        plain.push(character);
        rest = &after[end + 1..];
    }
    plain.push_str(rest);
    Ok(Cow::Owned(plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_text_and_namespaced_roots() {
        let root = parse(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- a comment -->\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name><Contents><Key>a</Key><LastModified>2020-08-21T20:24:12.000Z</LastModified><Size>1</Size><Empty/></Contents><Contents><Key>c</Key></Contents></ListBucketResult>\n",
        )
        .unwrap();
        assert_eq!(root.name, "ListBucketResult");
        assert_eq!(root.text_of("Name"), Some("b"));
        let contents: Vec<_> = root.children("Contents").collect();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].text_of("Key"), Some("a"));
        assert_eq!(
            contents[0].text_of("LastModified"),
            Some("2020-08-21T20:24:12.000Z")
        );
        assert_eq!(contents[0].text_of("Empty"), Some(""));
        assert_eq!(contents[1].text_of("Size"), None);
        assert_eq!(
            parse("<s3:Error a='1>2'><s3:Code>x</s3:Code></s3:Error>")
                .unwrap()
                .text_of("Code"),
            Some("x")
        );
    }

    #[test]
    fn entities_and_character_references_are_replaced() {
        assert_eq!(
            unescape("&quot;abc&quot; &amp;&lt;&gt;&apos; &#65;&#x42;&#X43;").unwrap(),
            "\"abc\" &<>' ABC"
        );
        assert!(matches!(unescape("plain").unwrap(), Cow::Borrowed("plain")));
        let root = parse("<Key>a &amp; b&#x2F;c</Key>").unwrap();
        assert_eq!(root.text, "a & b/c");
    }

    #[test]
    fn malformed_and_unsafe_documents_are_refused() {
        for bad in [
            "",
            "text",
            "<a>",
            "<a></b>",
            "</a>",
            "<a></a><b></b>",
            "<a/><b/>",
            "<a>&unknown;</a>",
            "<a>&#0;</a>",
            "<a>&#xD800;</a>",
            "<a>& alone</a>",
            "<a",
            "<?xml",
            "<!-- open",
            "<!DOCTYPE a [<!ENTITY e \"boom\">]><a>&e;</a>",
            "<a><![CDATA[x]]></a>",
            "<>",
            "<a/b>",
            "<a:>",
            "<a></a>tail",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let deep = format!(
            "{}{}",
            "<a>".repeat(DEEPEST + 1),
            "</a>".repeat(DEEPEST + 1)
        );
        assert!(parse(&deep).is_err());
        let fits = format!("{}{}", "<a>".repeat(DEEPEST), "</a>".repeat(DEEPEST));
        assert!(parse(&fits).is_ok());
    }

    /// arbitrary text never panics the reader, and what it accepts has names a tag can hold.
    #[test]
    fn fuzz_object_listings() {
        fn named(element: &Element) -> bool {
            !element.name.is_empty()
                && !element.name.contains(['<', '>', '/', ':', '&'])
                && element.children.iter().all(named)
        }
        bolero::check!().for_each(|bytes| {
            let text = String::from_utf8_lossy(bytes);
            if let Ok(plain) = unescape(&text) {
                assert!(plain.len() <= text.len(), "{text:?}");
            }
            if let Ok(root) = parse(&text) {
                assert!(named(&root), "{text:?}");
            }
        });
    }
}
