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
const MAX_PDF_INFO_BYTES: usize = 64 * 1024;

pub async fn extract(path: &Path) -> CoreResult<ExtractedDocument> {
    time::timeout(PDF_EXTRACT_TIMEOUT, extract_inner(path))
        .await
        .map_err(|_| {
            CoreError::public(
                ErrorCode::ExtractionFailed,
                "PDF 检测与文字提取超过 60 秒安全上限",
            )
        })?
}

async fn extract_inner(path: &Path) -> CoreResult<ExtractedDocument> {
    ensure_unencrypted(path).await?;
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
    let output = async {
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
    }
    .await?;

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

async fn ensure_unencrypted(path: &Path) -> CoreResult<()> {
    // pdftotext can successfully open an owner-encrypted PDF with an empty
    // user password. Check encryption explicitly before consuming its body.
    let mut child = Command::new("pdfinfo")
        .kill_on_drop(true)
        .env("LC_ALL", "C")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            CoreError::public(
                ErrorCode::ExtractionFailed,
                "PDF 检测工具不可用，请检查 Poppler 安装",
            )
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::public(ErrorCode::ExtractionFailed, "无法读取 PDF 检测结果"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::public(ErrorCode::ExtractionFailed, "无法读取 PDF 检测错误"))?;
    let (info, error_text) = tokio::try_join!(
        read_limited(&mut stdout, MAX_PDF_INFO_BYTES, ErrorCode::ExtractionFailed),
        read_limited(
            &mut stderr,
            MAX_PDF_ERROR_BYTES,
            ErrorCode::ExtractionFailed
        ),
    )?;
    let status = child.wait().await?;
    if !status.success() {
        let error = String::from_utf8_lossy(&error_text).to_ascii_lowercase();
        if error.contains("password") || error.contains("encrypted") {
            return Err(CoreError::public(ErrorCode::PdfEncrypted, "不支持加密 PDF"));
        }
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "PDF 检测失败",
        ));
    }
    validate_encryption_info(&String::from_utf8_lossy(&info))
}

fn validate_encryption_info(info: &str) -> CoreResult<()> {
    let flags: Vec<&str> = info
        .lines()
        .filter_map(|line| line.strip_prefix("Encrypted:"))
        .map(str::trim)
        .collect();
    if flags.iter().any(|value| value.starts_with("yes")) {
        return Err(CoreError::public(ErrorCode::PdfEncrypted, "不支持加密 PDF"));
    }
    // Reject ambiguous metadata instead of trusting a forged earlier line.
    if flags.as_slice() != ["no"] {
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "无法确认 PDF 加密状态",
        ));
    }
    Ok(())
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
    async fn rejects_encrypted_pdfs_including_an_empty_user_password() {
        let samples: &[&[u8]] = &[
            include_bytes!("../../../../tests/fixtures/owner-encrypted.pdf"),
            include_bytes!("../../../../tests/fixtures/password-encrypted.pdf"),
        ];
        for sample in samples {
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), sample).unwrap();
            let error = extract(file.path()).await.unwrap_err();
            assert_eq!(error.code(), ErrorCode::PdfEncrypted);
        }
    }

    #[tokio::test]
    async fn extracts_an_unencrypted_text_pdf_after_the_encryption_check() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            include_bytes!("../../../../tests/fixtures/e2e-sample.pdf"),
        )
        .unwrap();
        let document = extract(file.path()).await.unwrap();
        assert!(document.text.contains("心情很繁重"));
    }

    #[test]
    fn encryption_metadata_must_be_unambiguous_and_unencrypted() {
        assert!(validate_encryption_info("Title: sample\nEncrypted:   no\nPages: 1\n").is_ok());
        for info in [
            "Pages: 1\n",
            "Encrypted: unknown\n",
            "Encrypted: no\nEncrypted: no\n",
        ] {
            assert_eq!(
                validate_encryption_info(info).unwrap_err().code(),
                ErrorCode::ExtractionFailed
            );
        }
        assert_eq!(
            validate_encryption_info(
                "Title: forged\nEncrypted: no\nEncrypted: yes (algorithm: RC4)\n"
            )
            .unwrap_err()
            .code(),
            ErrorCode::PdfEncrypted
        );
    }

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
