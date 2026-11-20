use quick_xml::events::Event;
use quick_xml::{NsReader, XmlVersion};

/// An XML element by local name — namespace prefixes dropped, so the Strict
/// and transitional schemas read alike.
#[derive(Debug, Default)]
pub(super) struct Element {
    pub(super) line: usize,
    pub(super) foreign_namespace: bool,
    pub(super) name: String,
    attributes: Vec<(String, String)>,
    pub(super) children: Vec<Element>,
    pub(super) text: String,
}

impl Element {
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    pub(super) fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for child in &self.children {
            if child.name == name {
                out.push(child);
            }
            child.descendants(name, out);
        }
    }
}

fn element(
    start: &quick_xml::events::BytesStart<'_>,
    line: usize,
    foreign_namespace: bool,
) -> Element {
    let attributes = start
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let value = attribute.normalized_value(XmlVersion::Implicit1_0).ok()?;
            Some((
                attribute.key.local_name().as_ref().to_string(),
                value.into_owned(),
            ))
        })
        .collect();
    Element {
        name: start.local_name().as_ref().to_string(),
        attributes,
        line,
        foreign_namespace,
        ..Element::default()
    }
}

/// Parses one part into its root element.
pub(super) fn parse_xml(bytes: &[u8]) -> std::result::Result<Element, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = NsReader::from_str(text);
    let mut stack = vec![Element::default()];
    let mut offset = 0;
    let mut line = 1;
    loop {
        let pos = reader.buffer_position() as usize;
        line += text.as_bytes()[offset..pos]
            .iter()
            .filter(|&&c| c == b'\n')
            .count();
        offset = pos;
        if stack.len() > 256 {
            return Err("XML nesting exceeds 256 levels".into());
        }
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|error| error.to_string())?;
        let foreign = match ns {
            quick_xml::name::ResolveResult::Bound(ns) => {
                !ns.as_ref().ends_with("/reportdefinition")
            }
            quick_xml::name::ResolveResult::Unbound => false,
            quick_xml::name::ResolveResult::Unknown(_) => true,
        };
        match event {
            Event::Start(start) => stack.push(element(&start, line, foreign)),
            Event::Empty(empty) => {
                let element = element(&empty, line, foreign);
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .filter(|_| !stack.is_empty())
                    .ok_or("unbalanced end tag")?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Event::Text(text) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&text);
                }
            }
            Event::CData(data) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&data);
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(ch)) => Some(ch),
                    _ => match &*reference {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let (Some(ch), Some(current)) = (resolved, stack.last_mut()) {
                    current.text.push(ch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut document = stack
        .pop()
        .filter(|_| stack.is_empty())
        .ok_or("ends before its root element closes")?;
    if document.children.len() != 1 {
        return Err("no single root element".to_string());
    }
    Ok(document.children.remove(0))
}
