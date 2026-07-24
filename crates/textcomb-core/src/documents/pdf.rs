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
    if text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
        < 10
    {
        return Err(CoreError::public(
            ErrorCode::PdfScanned,
            "PDF 没有足够的可提取文字，可能是扫描件",
        ));
    }

    let mut lines = Vec::new();
    for (page_index, page) in text.split('\u{000c}').enumerate() {
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
