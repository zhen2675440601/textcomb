mod docx;
mod pdf;
mod txt;

use crate::error::{CoreError, CoreResult, ErrorCode};
use std::{io::Cursor, path::Path};
use textcomb_domain::{DocumentFormat, SourceLocation};

pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_TEXT_CHARS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSegment {
    pub char_start: usize,
    pub char_end: usize,
    pub page: Option<u32>,
    pub line: Option<u32>,
    pub paragraph_index: u32,
}

#[derive(Debug, Clone)]
pub struct ExtractedDocument {
    pub format: DocumentFormat,
    pub text: String,
    pub segments: Vec<SourceSegment>,
    pub char_count: usize,
}

impl ExtractedDocument {
    pub fn location(&self, start: usize, end: usize, quote: &str) -> CoreResult<SourceLocation> {
        if start >= end || end > self.char_count {
            return Err(CoreError::public(
                ErrorCode::ModelOutputInvalid,
                "模型问题位置超出正文范围",
            ));
        }
        let overlapping: Vec<&SourceSegment> = self
            .segments
            .iter()
            .filter(|segment| segment.char_start < end && segment.char_end > start)
            .collect();
        let first = overlapping.first().copied().ok_or_else(|| {
            CoreError::public(ErrorCode::ModelOutputInvalid, "问题位置无法映射到原文")
        })?;
        let last = overlapping.last().copied().unwrap_or(first);
        let paragraph_start = self
            .segments
            .iter()
            .find(|segment| segment.paragraph_index == first.paragraph_index)
            .map(|segment| segment.char_start)
            .unwrap_or(first.char_start);
        let sentence_index = self
            .text
            .chars()
            .skip(paragraph_start)
            .take(start.saturating_sub(paragraph_start))
            .filter(|character| matches!(character, '。' | '！' | '？' | '!' | '?'))
            .count() as u32;
        Ok(SourceLocation {
            document_format: self.format,
            page: first.page,
            line_start: first.line,
            line_end: last.line,
            paragraph_index: Some(first.paragraph_index),
            sentence_index: Some(sentence_index),
            char_start: start as u32,
            char_end: end as u32,
            quote: quote.to_owned(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct SourceLine {
    pub text: String,
    pub page: Option<u32>,
    pub line: Option<u32>,
    pub paragraph_break_after: bool,
}

pub async fn extract(path: &Path, format: DocumentFormat) -> CoreResult<ExtractedDocument> {
    let document = match format {
        DocumentFormat::Txt => txt::extract(path).await?,
        DocumentFormat::Docx => docx::extract(path).await?,
        DocumentFormat::Pdf => pdf::extract(path).await?,
    };
    if document.char_count > MAX_TEXT_CHARS {
        return Err(CoreError::public(
            ErrorCode::TextTooLong,
            format!("正文超过 {MAX_TEXT_CHARS} 字上限"),
        ));
    }
    if document.text.trim().is_empty() {
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "文档中没有可分析的正文",
        ));
    }
    Ok(document)
}

pub fn detect_format(filename: &str, bytes: &[u8]) -> CoreResult<DocumentFormat> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(CoreError::public(
            ErrorCode::FileTooLarge,
            "文件超过 20 MiB 上限",
        ));
    }
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| CoreError::public(ErrorCode::UnsupportedFormat, "文件缺少可识别的扩展名"))?;
    match extension.as_str() {
        "txt" => Ok(DocumentFormat::Txt),
        "docx" if is_docx(bytes) => Ok(DocumentFormat::Docx),
        "pdf" if bytes.starts_with(b"%PDF-") => Ok(DocumentFormat::Pdf),
        "docx" | "pdf" => Err(CoreError::public(
            ErrorCode::UnsupportedFormat,
            "文件内容与扩展名不一致",
        )),
        _ => Err(CoreError::public(
            ErrorCode::UnsupportedFormat,
            "仅支持 TXT、DOCX 和文字型 PDF",
        )),
    }
}

pub fn validate_media_type(format: DocumentFormat, media_type: &str) -> CoreResult<()> {
    let media_type = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let allowed = media_type.is_empty()
        || media_type == "application/octet-stream"
        || match format {
            DocumentFormat::Txt => media_type == "text/plain",
            DocumentFormat::Docx => {
                media_type
                    == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    || media_type == "application/zip"
            }
            DocumentFormat::Pdf => media_type == "application/pdf",
        };
    if !allowed {
        return Err(CoreError::public(
            ErrorCode::UnsupportedFormat,
            "文件 MIME 类型与扩展名或内容不一致",
        ));
    }
    Ok(())
}

fn is_docx(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK") {
        return false;
    }
    let Ok(archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    let mut has_content_types = false;
    let mut has_document = false;
    for name in archive.file_names() {
        has_content_types |= name == "[Content_Types].xml";
        has_document |= name == "word/document.xml";
    }
    has_content_types && has_document
}

pub(crate) fn build_document(format: DocumentFormat, lines: Vec<SourceLine>) -> ExtractedDocument {
    let mut text = String::new();
    let mut segments = Vec::new();
    let mut char_cursor = 0_usize;
    let mut paragraph_index = 0_u32;
    let total = lines.len();

    for (index, line) in lines.into_iter().enumerate() {
        let line_chars = line.text.chars().count();
        if line_chars > 0 {
            text.push_str(&line.text);
            segments.push(SourceSegment {
                char_start: char_cursor,
                char_end: char_cursor + line_chars,
                page: line.page,
                line: line.line,
                paragraph_index,
            });
            char_cursor += line_chars;
        }
        if index + 1 < total {
            text.push('\n');
            char_cursor += 1;
            if line.paragraph_break_after {
                text.push('\n');
                char_cursor += 1;
                paragraph_index += 1;
            }
        }
        if line.text.trim().is_empty() {
            paragraph_index += 1;
        }
    }

    ExtractedDocument {
        format,
        text,
        segments,
        char_count: char_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_detection_checks_signature() {
        assert_eq!(
            detect_format("a.pdf", b"%PDF-1.7").unwrap(),
            DocumentFormat::Pdf
        );
        assert!(detect_format("a.pdf", b"not-pdf").is_err());
        assert_eq!(
            detect_format("a.txt", "正文".as_bytes()).unwrap(),
            DocumentFormat::Txt
        );
    }

    #[test]
    fn source_location_maps_lines() {
        let document = build_document(
            DocumentFormat::Txt,
            vec![
                SourceLine {
                    text: "第一行。".to_owned(),
                    page: None,
                    line: Some(1),
                    paragraph_break_after: false,
                },
                SourceLine {
                    text: "第二行".to_owned(),
                    page: None,
                    line: Some(2),
                    paragraph_break_after: false,
                },
            ],
        );
        let location = document.location(5, 8, "第二行").unwrap();
        assert_eq!(location.line_start, Some(2));
        assert_eq!(location.sentence_index, Some(1));
    }
}
