use super::{ExtractedDocument, SourceLine, build_document};
use crate::error::{CoreError, CoreResult, ErrorCode};
use std::{path::Path, process::Stdio};
use textcomb_domain::DocumentFormat;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time,
};

const PDF_EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_PDF_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PDF_ERROR_BYTES: usize = 16 * 1024;

pub async fn extract(path: &Path) -> CoreResult<ExtractedDocument> {
    let mut command = Command::new("pdftotext");
    command
        .kill_on_drop(true)
        .arg("-enc")
        .arg("UTF-8")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = time::timeout(PDF_EXTRACT_TIMEOUT, async {
        let mut child = command.spawn()?;
        let mut stdout = child.stdout.take().ok_or_else(|| {
            CoreError::public(ErrorCode::ExtractionFailed, "无法读取 PDF 提取结果")
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            CoreError::public(ErrorCode::ExtractionFailed, "无法读取 PDF 提取错误")
        })?;
        let (text, error_text) = tokio::try_join!(
            read_limited(&mut stdout, MAX_PDF_TEXT_BYTES, ErrorCode::TextTooLong),
            read_limited(
                &mut stderr,
                MAX_PDF_ERROR_BYTES,
                ErrorCode::ExtractionFailed
            ),
        )?;
        let status = child.wait().await?;
        Ok::<_, CoreError>((status, text, error_text))
    })
    .await
    .map_err(|_| {
        CoreError::public(
            ErrorCode::ExtractionFailed,
            "PDF 文字提取超过 60 秒安全上限",
        )
    })??;

    if !output.0.success() {
        let stderr = String::from_utf8_lossy(&output.2).to_ascii_lowercase();
        if stderr.contains("password") || stderr.contains("encrypted") {
            return Err(CoreError::public(ErrorCode::PdfEncrypted, "不支持加密 PDF"));
        }
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "PDF 文字提取失败",
        ));
    }
    let text = String::from_utf8(output.1).map_err(|_| {
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

async fn read_limited<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
    overflow_code: ErrorCode,
) -> CoreResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > limit {
            return Err(CoreError::public(overflow_code, "PDF 提取输出超过安全上限"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn streaming_reader_rejects_oversized_output() {
        let (mut sender, mut receiver) = duplex(16);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            sender.write_all(b"123456789").await.unwrap();
        });
        let error = read_limited(&mut receiver, 8, ErrorCode::TextTooLong)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::TextTooLong);
        writer.await.unwrap();
    }
}
