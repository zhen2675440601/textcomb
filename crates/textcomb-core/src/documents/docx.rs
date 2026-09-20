use super::{ExtractedDocument, SourceLine, build_document};
use crate::error::{CoreError, CoreResult, ErrorCode};
use quick_xml::{Reader, events::Event};
use std::{fs::File, io::Read, path::Path};
use textcomb_domain::DocumentFormat;
use zip::ZipArchive;

const MAX_ARCHIVE_ENTRIES: usize = 1_000;
const MAX_UNCOMPRESSED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_DOCUMENT_XML_BYTES: u64 = 10 * 1024 * 1024;

pub async fn extract(path: &Path) -> CoreResult<ExtractedDocument> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || extract_blocking(&path))
        .await
        .map_err(|error| CoreError::Internal(anyhow::Error::new(error)))?
}

fn extract_blocking(path: &Path) -> CoreResult<ExtractedDocument> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|_| CoreError::public(ErrorCode::ExtractionFailed, "DOCX 压缩结构无效"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "DOCX 包含过多文件，已拒绝处理",
        ));
    }
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| CoreError::public(ErrorCode::ExtractionFailed, "DOCX 压缩目录无效"))?;
        total_size = total_size.checked_add(entry.size()).ok_or_else(|| {
            CoreError::public(ErrorCode::ExtractionFailed, "DOCX 解压后体积超过安全上限")
        })?;
        if total_size > MAX_UNCOMPRESSED_BYTES {
            return Err(CoreError::public(
                ErrorCode::ExtractionFailed,
                "DOCX 解压后体积超过安全上限",
            ));
        }
    }

    let mut document_xml = archive.by_name("word/document.xml").map_err(|_| {
        CoreError::public(ErrorCode::ExtractionFailed, "DOCX 中缺少 word/document.xml")
    })?;
    if document_xml.size() > MAX_DOCUMENT_XML_BYTES {
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "DOCX 正文 XML 超过安全上限",
        ));
    }
    let mut xml = Vec::with_capacity(document_xml.size() as usize);
    document_xml.read_to_end(&mut xml)?;
    parse_document_xml(&xml)
}

fn parse_document_xml(xml: &[u8]) -> CoreResult<ExtractedDocument> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut paragraphs = Vec::new();
    let mut paragraph = String::new();
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut deleted_depth = 0_usize;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) if is_tag(event.name().as_ref(), b"del") => {
                deleted_depth += 1;
            }
            Ok(Event::End(event)) if is_tag(event.name().as_ref(), b"del") => {
                deleted_depth = deleted_depth.saturating_sub(1);
            }
            Ok(Event::Start(event)) if is_tag(event.name().as_ref(), b"p") => {
                in_paragraph = true;
                paragraph.clear();
            }
            Ok(Event::End(event)) if is_tag(event.name().as_ref(), b"p") => {
                if in_paragraph {
                    paragraphs.push(std::mem::take(&mut paragraph));
                    in_paragraph = false;
                }
            }
            Ok(Event::Start(event)) if is_tag(event.name().as_ref(), b"t") => {
                in_text = true;
            }
            Ok(Event::End(event)) if is_tag(event.name().as_ref(), b"t") => {
                in_text = false;
            }
            Ok(Event::Empty(event))
                if in_paragraph && deleted_depth == 0 && is_tag(event.name().as_ref(), b"tab") =>
            {
                paragraph.push('\t');
            }
            Ok(Event::Empty(event))
                if in_paragraph && deleted_depth == 0 && is_tag(event.name().as_ref(), b"br") =>
            {
                paragraph.push('\n');
            }
            Ok(Event::Text(event)) if in_paragraph && in_text && deleted_depth == 0 => {
                let value = event.decode().map_err(|_| {
                    CoreError::public(ErrorCode::ExtractionFailed, "DOCX 正文编码无效")
                })?;
                paragraph.push_str(&value);
            }
            Ok(Event::GeneralRef(event)) if in_paragraph && in_text && deleted_depth == 0 => {
                let name = event.decode().map_err(|_| {
                    CoreError::public(ErrorCode::ExtractionFailed, "DOCX 实体引用编码无效")
                })?;
                paragraph.push_str(&decode_general_reference(&name));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => {
                return Err(CoreError::public(
                    ErrorCode::ExtractionFailed,
                    "DOCX 正文 XML 无法解析",
                ));
            }
        }
        buffer.clear();
    }

    let last_non_empty = paragraphs
        .iter()
        .rposition(|paragraph| !paragraph.trim().is_empty())
        .unwrap_or(0);
    let lines = paragraphs
        .into_iter()
        .take(last_non_empty + 1)
        .enumerate()
        .map(|(index, text)| SourceLine {
            text,
            page: None,
            line: None,
            paragraph_break_after: index < last_non_empty,
        })
        .collect();
    Ok(build_document(DocumentFormat::Docx, lines))
}

fn is_tag(name: &[u8], local: &[u8]) -> bool {
    name == local || name.strip_prefix(b"w:") == Some(local)
}

fn decode_general_reference(name: &str) -> String {
    let decoded = match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        value
            if value
                .strip_prefix("#x")
                .or_else(|| value.strip_prefix("#X"))
                .is_some() =>
        {
            value
                .strip_prefix("#x")
                .or_else(|| value.strip_prefix("#X"))
                .and_then(|digits| u32::from_str_radix(digits, 16).ok())
                .and_then(char::from_u32)
        }
        value if value.starts_with('#') => value
            .strip_prefix('#')
            .and_then(|digits| digits.parse::<u32>().ok())
            .and_then(char::from_u32),
        _ => None,
    };
    decoded.map_or_else(|| format!("&{name};"), |character| character.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs_and_skips_deleted_text() {
        let xml = r#"
            <w:document xmlns:w="x"><w:body>
              <w:p><w:r><w:t>第一段</w:t></w:r></w:p>
              <w:p><w:del><w:r><w:t>删除</w:t></w:r></w:del><w:r><w:t>保留</w:t></w:r></w:p>
            </w:body></w:document>
        "#;
        let document = parse_document_xml(xml.as_bytes()).unwrap();
        assert_eq!(document.text, "第一段\n\n保留");
    }

    #[test]
    fn decodes_xml_entities_without_losing_source_characters() {
        let xml = r#"
            <w:document xmlns:w="x"><w:body>
              <w:p><w:r><w:t>A&amp;B&#x4e2d;&#25991;</w:t></w:r></w:p>
            </w:body></w:document>
        "#;
        let document = parse_document_xml(xml.as_bytes()).unwrap();
        assert_eq!(document.text, "A&B中文");
    }
}
