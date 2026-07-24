use super::{ExtractedDocument, SourceLine, build_document};
use crate::error::{CoreError, CoreResult, ErrorCode};
use encoding_rs::Encoding;
use std::path::Path;
use textcomb_domain::DocumentFormat;
use tokio::fs;

pub async fn extract(path: &Path) -> CoreResult<ExtractedDocument> {
    let bytes = fs::read(path).await?;
    let decoded = match std::str::from_utf8(&bytes) {
        Ok(value) => value.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => {
            let encoding = Encoding::for_label(b"gb18030").ok_or_else(|| {
                CoreError::public(ErrorCode::ExtractionFailed, "当前运行时不支持 GB18030")
            })?;
            let (value, _, had_errors) = encoding.decode(&bytes);
            if had_errors {
                return Err(CoreError::public(
                    ErrorCode::ExtractionFailed,
                    "TXT 既不是有效 UTF-8，也无法可靠按 GB18030 解码",
                ));
            }
            value.into_owned()
        }
    };

    let normalized = decoded.replace("\r\n", "\n").replace('\r', "\n");
    let raw_lines: Vec<&str> = normalized.lines().collect();
    let last_non_empty = raw_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(0);
    let lines = raw_lines
        .into_iter()
        .take(last_non_empty + 1)
        .enumerate()
        .map(|(index, line)| SourceLine {
            text: line.to_owned(),
            page: None,
            line: Some((index + 1) as u32),
            paragraph_break_after: false,
        })
        .collect();
    Ok(build_document(DocumentFormat::Txt, lines))
}
