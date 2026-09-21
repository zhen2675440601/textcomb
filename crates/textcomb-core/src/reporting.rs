use crate::error::{CoreError, CoreResult, ErrorCode};
use std::path::{Path, PathBuf};
use textcomb_domain::{AnalysisProfile, Issue, IssueLevel, ReportV1, SourceLocation};
use tokio::{fs, process::Command, time};

pub fn to_markdown(report: &ReportV1) -> String {
    let mut output = String::new();
    output.push_str("# 文梳问题报告\n\n");
    output.push_str(&format!("- 文件：{}\n", report.document.original_name));
    output.push_str(&format!("- 正文字数：{}\n", report.document.char_count));
    output.push_str(&format!(
        "- 分析场景：{}\n",
        analysis_profile_label(report.analysis.analysis_profile)
    ));
    output.push_str(&format!("- 正式问题：{}\n", report.summary.confirmed));
    output.push_str(&format!("- 疑似问题：{}\n", report.summary.suspected));
    output.push_str(&format!(
        "- 模型：{} / {}\n",
        report.analysis.candidate_model, report.analysis.verifier_model
    ));
    output.push_str(&format!(
        "- 提示词版本：{}\n\n",
        report.analysis.prompt_version
    ));
    if report.issues.is_empty() {
        output.push_str("未发现需要报告的问题。最终结果仍请作者自行审核。\n");
        return output;
    }
    for (index, issue) in report.issues.iter().enumerate() {
        output.push_str(&format!(
            "## {}. {} · {}\n\n",
            index + 1,
            category_label(issue),
            level_label(issue.level)
        ));
        output.push_str(&format!("- 位置：{}\n", location_label(&issue.location)));
        output.push_str(&format!("- 原文：{}\n", issue.original_text));
        output.push_str(&format!("- 原因：{}\n", issue.reason));
        output.push_str(&format!("- 建议：{}\n", issue.suggestion));
        output.push_str(&format!("- 置信度：{}%\n", issue.confidence));
        if !issue.evidence_refs.is_empty() {
            output.push_str("- 依据：\n");
            for evidence in &issue.evidence_refs {
                output.push_str(&format!(
                    "  - [{} {}]({})\n",
                    evidence.title, evidence.revision, evidence.source_url
                ));
            }
        }
        output.push('\n');
    }
    output.push_str("> 文梳提供辅助建议，最终判断由作者完成。\n");
    output
}

pub fn to_typst(report: &ReportV1) -> String {
    let mut output = String::from(
        r#"#set document(title: "文梳问题报告")
#set text(lang: "zh", size: 10.5pt)
#set page(margin: 22mm)
#set heading(numbering: "1.")

= 文梳问题报告

"#,
    );
    output.push_str(&format!(
        "#table(columns: (80pt, 1fr), [文件], [{}], [正文字数], [{}], [分析场景], [{}], [正式问题], [{}], [疑似问题], [{}])\n\n",
        typst_escape(&report.document.original_name),
        report.document.char_count,
        typst_escape(analysis_profile_label(report.analysis.analysis_profile)),
        report.summary.confirmed,
        report.summary.suspected
    ));
    for (index, issue) in report.issues.iter().enumerate() {
        output.push_str(&format!(
            "== {}. {} · {}\n\n",
            index + 1,
            category_label(issue),
            level_label(issue.level)
        ));
        output.push_str(&format!(
            "- *位置：* {}\n- *原文：* #quote[{}]\n- *原因：* {}\n- *建议：* {}\n- *置信度：* {}%\n",
            typst_escape(&location_label(&issue.location)),
            typst_escape(&issue.original_text),
            typst_escape(&issue.reason),
            typst_escape(&issue.suggestion),
            issue.confidence
        ));
        if !issue.evidence_refs.is_empty() {
            output.push_str("- *依据：*\n");
            for evidence in &issue.evidence_refs {
                output.push_str(&format!(
                    "  - #link(\"{}\")[{} {}]\n",
                    typst_escape(&evidence.source_url),
                    typst_escape(&evidence.title),
                    typst_escape(&evidence.revision)
                ));
            }
        }
        output.push('\n');
    }
    output
        .push_str("#block(inset: 8pt, fill: luma(240))[文梳提供辅助建议，最终判断由作者完成。]\n");
    output
}

pub async fn render_pdf(
    report: &ReportV1,
    output_path: &Path,
    temporary_path: &Path,
) -> CoreResult<PathBuf> {
    fs::write(temporary_path, to_typst(report)).await?;
    let mut command = Command::new("typst");
    command
        .kill_on_drop(true)
        .arg("compile")
        .arg(temporary_path)
        .arg(output_path);
    let result = time::timeout(std::time::Duration::from_secs(60), command.output()).await;
    let _ = fs::remove_file(temporary_path).await;
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let _ = fs::remove_file(output_path).await;
            return Err(error.into());
        }
        Err(_) => {
            let _ = fs::remove_file(output_path).await;
            return Err(CoreError::public(
                ErrorCode::ReportRenderFailed,
                "PDF 报告生成超过 60 秒",
            ));
        }
    };
    if !result.status.success() {
        let _ = fs::remove_file(output_path).await;
        return Err(CoreError::public(
            ErrorCode::ReportRenderFailed,
            "Typst 无法生成 PDF 报告",
        ));
    }
    Ok(output_path.to_path_buf())
}

fn category_label(issue: &Issue) -> &'static str {
    match issue.category {
        textcomb_domain::IssueCategory::Typo => "错字",
        textcomb_domain::IssueCategory::Punctuation => "标点",
        textcomb_domain::IssueCategory::Grammar => "病句",
        textcomb_domain::IssueCategory::Paragraph => "分段建议",
    }
}

fn analysis_profile_label(profile: AnalysisProfile) -> &'static str {
    match profile {
        AnalysisProfile::General => "通用文章",
        AnalysisProfile::Academic => "学术论文",
        AnalysisProfile::Financial => "财报/商业报告",
    }
}

fn level_label(level: IssueLevel) -> &'static str {
    match level {
        IssueLevel::Confirmed => "正式问题",
        IssueLevel::Suspected => "疑似问题",
    }
}

fn location_label(location: &SourceLocation) -> String {
    if let Some(page) = location.page {
        return format!(
            "第 {} 页，第 {}–{} 行",
            page,
            location.line_start.unwrap_or(1),
            location
                .line_end
                .unwrap_or(location.line_start.unwrap_or(1))
        );
    }
    if location.document_format == textcomb_domain::DocumentFormat::Txt {
        return format!(
            "第 {}–{} 行",
            location.line_start.unwrap_or(1),
            location
                .line_end
                .unwrap_or(location.line_start.unwrap_or(1))
        );
    }
    format!(
        "第 {} 段，第 {} 句",
        location.paragraph_index.unwrap_or(0) + 1,
        location.sentence_index.unwrap_or(0) + 1
    )
}

fn typst_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('#', "\\#")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('$', "\\$")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::time::OffsetDateTime;
    use textcomb_domain::{
        AnalysisProfile, AnalysisSnapshot, DocumentFormat, DocumentMetadata, ReportV1,
    };
    use uuid::Uuid;

    #[test]
    fn empty_report_exports_readable_markdown() {
        let report = ReportV1::new(
            Uuid::nil(),
            Uuid::nil(),
            DocumentMetadata {
                original_name: "测试.txt".to_owned(),
                format: DocumentFormat::Txt,
                char_count: 10,
            },
            AnalysisSnapshot {
                analysis_profile: AnalysisProfile::General,
                provider_kind: "openai_compatible".to_owned(),
                candidate_model: "model".to_owned(),
                verifier_model: "model".to_owned(),
                prompt_version: "v1".to_owned(),
                analyzer_version: "v1".to_owned(),
                reference_profile: false,
            },
            vec![],
            OffsetDateTime::UNIX_EPOCH,
        );
        let markdown = to_markdown(&report);
        assert!(markdown.contains("未发现"));
        assert!(to_typst(&report).contains("文梳问题报告"));
    }
}
