use quick_xml::{Reader, events::Event};
use std::collections::HashMap;

pub(super) fn image_list_is_empty(output: &[u8]) -> bool {
    let Ok(output) = std::str::from_utf8(output) else {
        return false;
    };
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return false;
    };
    let expected = [
        "page", "num", "type", "width", "height", "color", "comp", "bpc", "enc", "interp",
        "object", "ID", "x-ppi", "y-ppi", "size", "ratio",
    ];
    if !header.split_ascii_whitespace().eq(expected) {
        return false;
    }
    let Some(separator) = lines.next() else {
        return false;
    };
    separator.trim().bytes().all(|byte| byte == b'-') && lines.next().is_none()
}

// This is a conservative check of Poppler/Cairo's generated geometry, not a
// general SVG interpreter. Unknown paint, images, clipping and outlines outside
// font symbols cannot prove that pdftotext covered a sparse page's contents.
pub(super) fn confirms_sparse_text(svg: &[u8], expected_characters: usize) -> bool {
    let mut reader = Reader::from_reader(svg);
    let mut buffer = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut roots = 0;
    let mut current_symbol = None;
    let mut symbols = HashMap::new();
    let mut references = Vec::new();
    loop {
        let (element, empty) = match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => (element, false),
            Ok(Event::Empty(element)) => (element, true),
            Ok(Event::End(element)) => {
                if stack.pop().as_deref() != Some(element.name().as_ref()) {
                    return false;
                }
                if element.name().as_ref() == b"symbol" {
                    current_symbol = None;
                }
                buffer.clear();
                continue;
            }
            Ok(Event::Text(text)) if text.iter().all(u8::is_ascii_whitespace) => {
                buffer.clear();
                continue;
            }
            Ok(Event::Decl(_) | Event::Comment(_)) => {
                buffer.clear();
                continue;
            }
            Ok(Event::Eof) => break,
            _ => return false,
        };
        let name = element.name().as_ref().to_vec();
        let in_definitions = stack.iter().any(|name| name == b"defs");
        match name.as_slice() {
            b"svg" => {
                if !stack.is_empty()
                    || roots != 0
                    || attribute(&element, b"xmlns").as_deref()
                        != Some(b"http://www.w3.org/2000/svg")
                {
                    return false;
                }
                roots += 1;
            }
            b"defs" if stack.last().map(Vec::as_slice) == Some(b"svg") => {}
            b"g" if !stack.is_empty() => {}
            b"symbol" if in_definitions && current_symbol.is_none() => {
                let Some(id) = attribute(&element, b"id") else {
                    return false;
                };
                if !is_glyph_id(&id) || symbols.insert(id.clone(), false).is_some() {
                    return false;
                }
                if !empty {
                    current_symbol = Some(id);
                }
            }
            b"path" if current_symbol.is_some() && in_definitions => {
                let Some(path) = attribute(&element, b"d") else {
                    return false;
                };
                if !path.iter().all(u8::is_ascii_whitespace)
                    && let Some(ink) = current_symbol.as_ref().and_then(|id| symbols.get_mut(id))
                {
                    *ink = true;
                }
            }
            b"use" if !in_definitions && current_symbol.is_none() && !stack.is_empty() => {
                let Some(reference) =
                    attribute(&element, b"xlink:href").or_else(|| attribute(&element, b"href"))
                else {
                    return false;
                };
                let Some(id) = reference.strip_prefix(b"#") else {
                    return false;
                };
                if !is_glyph_id(id) {
                    return false;
                }
                references.push(id.to_vec());
            }
            _ => return false,
        }
        if !empty {
            stack.push(name);
        }
        buffer.clear();
    }
    if roots != 1 || !stack.is_empty() {
        return false;
    }
    let mut painted_characters = 0;
    for id in references {
        let Some(painted) = symbols.get(&id) else {
            return false;
        };
        painted_characters += usize::from(*painted);
    }
    painted_characters == expected_characters
}

fn attribute(element: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<Vec<u8>> {
    Some(element.try_get_attribute(name).ok()??.value.into_owned())
}

fn is_glyph_id(id: &[u8]) -> bool {
    let Some(suffix) = id.strip_prefix(b"glyph") else {
        return false;
    };
    let parts: Vec<_> = suffix.split(|byte| *byte == b'-').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> Vec<u8> {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{body}</svg>"#).into_bytes()
    }

    #[test]
    fn image_metadata_must_confirm_no_raster_paint_before_rendering() {
        let header = b"page num type width height color comp bpc enc interp object ID x-ppi y-ppi size ratio\n----\n";
        assert!(image_list_is_empty(header));
        let mut scan = header.to_vec();
        scan.extend_from_slice(b"2 0 image 100000 100000 rgb 3 8 jpeg no 4 0 72 72 1K 0.1%\n");
        assert!(!image_list_is_empty(&scan));
        for malformed in [b"".as_slice(), b"----\n", b"page\n----\n", b"\xff"] {
            assert!(!image_list_is_empty(malformed));
        }
    }

    #[test]
    fn requires_matching_rendered_glyphs_or_a_confirmed_empty_page() {
        assert!(confirms_sparse_text(&svg("<g/>"), 0));
        assert!(!confirms_sparse_text(&svg("<g/>"), 1));
        let text = svg(
            r##"<defs><g><symbol id="glyph0-1"><path d="M 0 0 L 1 1"/></symbol><symbol id="glyph0-2"><path d=""/></symbol></g></defs><g><use href="#glyph0-1"/><use href="#glyph0-2"/><use href="#glyph0-1"/></g>"##,
        );
        assert!(confirms_sparse_text(&text, 2));
        assert!(!confirms_sparse_text(&text, 1));
        assert!(!confirms_sparse_text(&text, 3));
    }

    #[test]
    fn unknown_images_outlines_and_ambiguous_geometry_are_not_blank_or_text_only() {
        for body in [
            r#"<g><image href="data:image/png;base64,AA=="/></g>"#,
            r#"<g><path d="M 0 0 L 1 1"/></g>"#,
            r##"<g><use href="#glyph0-1"/></g>"##,
            r#"<defs><symbol id="glyph0-1"/><symbol id="glyph0-1"/></defs>"#,
            r#"<defs><symbol id="image0"><path d="M 0 0"/></symbol></defs>"#,
            r#"<defs><clipPath id="clip1"><path d="M 0 0"/></clipPath></defs>"#,
            r#"<g><rect width="1" height="1"/></g>"#,
            r#"<g><text>hidden body</text></g>"#,
            r#"<g>unexpected text</g>"#,
        ] {
            assert!(!confirms_sparse_text(&svg(body), 0), "{body}");
        }
        for malformed in [
            b"".as_slice(),
            b"<svg><g/></svg>",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g>",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>",
            b"<!DOCTYPE svg><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ] {
            assert!(!confirms_sparse_text(malformed, 0));
        }
    }
}
