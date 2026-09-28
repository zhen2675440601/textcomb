use super::{ExtractedDocument, SourceLine, build_document};
use crate::error::{CoreError, CoreResult, ErrorCode};
use std::path::Path;
use textcomb_domain::DocumentFormat;
use tokio::{process::Command, time};

const PDF_EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_PDF_TEXT_BYTES: usize = 8 * 1024 * 1024;

pub async fn extract(path: &Path) -> CoreResult<ExtractedDocument> {
    let mut command = Command::new("pdftotext");
    command
        .kill_on_drop(true)
        .arg("-enc")
        .arg("UTF-8")
        .arg("-layout")
        .arg(path)
        .arg("-");
    let output = time::timeout(PDF_EXTRACT_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            CoreError::public(
                ErrorCode::ExtractionFailed,
                "PDF 文字提取超过 60 秒安全上限",
            )
        })??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("password") || stderr.contains("encrypted") {
            return Err(CoreError::public(ErrorCode::PdfEncrypted, "不支持加密 PDF"));
        }
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "PDF 文字提取失败",
        ));
    }
    if output.stdout.len() > MAX_PDF_TEXT_BYTES {
        return Err(CoreError::public(
            ErrorCode::TextTooLong,
            "PDF 提取结果超过安全上限",
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| {
        CoreError::public(ErrorCode::ExtractionFailed, "PDF 提取结果不是有效 UTF-8")
    })?;
    let pages = validate_text_pages(&text)?;
    let mut lines = Vec::new();
    for (page_index, page) in pages.into_iter().enumerate() {
        let page_lines: Vec<&str> = page.lines().collect();
        for (line_index, line) in page_lines.iter().enumerate() {
            lines.push(SourceLine {
                text: line.trim_end().to_owned(),
                page: Some((page_index + 1) as u32),
                line: Some((line_index + 1) as u32),
                paragraph_break_after: line.trim().is_empty(),
            });
        }
    }
    while lines.last().is_some_and(|line| line.text.trim().is_empty()) {
        lines.pop();
    }
    Ok(build_document(DocumentFormat::Pdf, lines))
}

fn validate_text_pages(text: &str) -> CoreResult<Vec<&str>> {
    let mut pages: Vec<&str> = text.split('\u{000c}').collect();
    // pdftotext normally appends a form feed after the final page.
    if pages.last().is_some_and(|page| page.trim().is_empty()) {
        pages.pop();
    }
    for (index, page) in pages.iter().enumerate() {
        if page
            .chars()
            .filter(|character| !character.is_whitespace())
            .count()
            < 10
        {
            return Err(CoreError::public(
                ErrorCode::PdfScanned,
                format!(
                    "PDF 第 {} 页可提取文字不足，无法确认已覆盖全文；请改用 TXT 或 DOCX",
                    index + 1
                ),
            ));
        }
    }
    if pages.is_empty() {
        return Err(CoreError::public(
            ErrorCode::PdfScanned,
            "PDF 没有可提取的文字",
        ));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_scanned_page_after_a_text_cover() {
        let error = validate_text_pages("这是一页真实的文章正文。\u{000c}\u{000c}").unwrap_err();
        assert_eq!(error.code(), ErrorCode::PdfScanned);
        assert!(error.safe_message().contains("第 2 页"));
    }

    #[test]
    fn ignores_trailing_form_feed_after_last_text_page() {
        assert_eq!(
            validate_text_pages("这是一页真实的文章正文。\u{000c}")
                .unwrap()
                .len(),
            1
        );
    }
}
