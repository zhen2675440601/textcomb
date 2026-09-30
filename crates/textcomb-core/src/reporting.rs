use crate::error::{CoreError, CoreResult, ErrorCode};
use std::path::{Path, PathBuf};
use textcomb_domain::{
    AnalysisProfile, FeedbackVerdict, Issue, IssueLevel, ReportV1, SourceLocation,
};
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
    output.push_str(&format!("- 模型接口：{}\n", report.analysis.provider_kind));
    output.push_str(&format!(
        "- 模型：{} / {}\n",
        report.analysis.candidate_model, report.analysis.verifier_model
    ));
    output.push_str(&format!(
        "- 提示词版本：{}\n",
        report.analysis.prompt_version
    ));
    output.push_str(&format!("- 依据库版本：{}\n", reference_label(report)));
    output.push_str(&format!(
        "- 分析器版本：{}\n",
        report.analysis.analyzer_version
    ));
    output.push_str(&format!("- 模型配置：{}\n\n", profile_label(report)));
    if report.issues.is_empty() {
        output.push_str("未发现需要报告的问题。最终结果仍请作者自行审核。\n\n");
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
        if let Some(feedback) = issue.feedback {
            output.push_str(&format!("- 作者审核：{}\n", feedback_label(feedback)));
        }
        if !issue.evidence_refs.is_empty() {
            output.push_str("- 参考资料（具体条款未核对）：\n");
            for evidence in &issue.evidence_refs {
                output.push_str(&format!(
                    "  - [{} {}]({})\n",
                    evidence.title, evidence.revision, evidence.source_url
                ));
            }
        }
        output.push('\n');
    }
    if !report.missed_issues.is_empty() {
        output.push_str("## 作者标记的漏检\n\n");
        for miss in &report.missed_issues {
            output.push_str(&format!(
                "- {}（字符 {}–{}）：{}；说明：{}\n",
                miss.category.as_str(),
                miss.char_start,
                miss.char_end,
                miss.quote,
                miss.note
            ));
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
    output.push_str(&format!(
        "- *模型接口：* {}\n- *模型：* {} / {}\n- *提示词版本：* {}\n- *依据库版本：* {}\n- *分析器版本：* {}\n- *模型配置：* {}\n\n",
        typst_escape(&report.analysis.provider_kind),
        typst_escape(&report.analysis.candidate_model),
        typst_escape(&report.analysis.verifier_model),
        typst_escape(&report.analysis.prompt_version),
        typst_escape(reference_label(report)),
        typst_escape(&report.analysis.analyzer_version),
        profile_label(report),
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
        if let Some(feedback) = issue.feedback {
            output.push_str(&format!("- *作者审核：* {}\n", feedback_label(feedback)));
        }
        if !issue.evidence_refs.is_empty() {
            output.push_str("- *参考资料（具体条款未核对）：*\n");
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
    if !report.missed_issues.is_empty() {
        output.push_str("== 作者标记的漏检\n\n");
        for miss in &report.missed_issues {
            output.push_str(&format!(
                "- {}（字符 {}–{}）：#quote[{}]；说明：{}\n",
                typst_escape(miss.category.as_str()),
                miss.char_start,
                miss.char_end,
                typst_escape(&miss.quote),
                typst_escape(&miss.note)
            ));
        }
        output.push('\n');
    }
    output
        .push_str("#block(inset: 8pt, fill: luma(240))[文梳提供辅助建议，最终判断由作者完成。]\n");
    output
}

fn reference_label(report: &ReportV1) -> &str {
    if report.analysis.reference_version.is_empty() {
        "旧报告未记录"
    } else {
        &report.analysis.reference_version
    }
}

fn profile_label(report: &ReportV1) -> &'static str {
    if report.analysis.reference_profile {
        "参考配置"
    } else {
        "未经验证"
    }
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

fn feedback_label(feedback: FeedbackVerdict) -> &'static str {
    match feedback {
        FeedbackVerdict::Correct => "正确",
        FeedbackVerdict::Incorrect => "错误",
        FeedbackVerdict::Disputed => "有争议",
    }
}

fn location_label(location: &SourceLocation) -> String {
    if let Some(page) = location.page {
        let start = location.line_start.unwrap_or(1);
        let end = location.line_end.unwrap_or(start);
        if let Some(end_page) = location.page_end
            && end_page != page
        {
            return format!("第 {page} 页第 {start} 行至第 {end_page} 页第 {end} 行");
        }
        if location.page_end.is_none() && end < start {
            return format!("第 {page} 页第 {start} 行起（结束页未记录）");
        }
        return if start == end {
            format!("第 {page} 页，第 {start} 行")
        } else {
            format!("第 {page} 页，第 {start}–{end} 行")
        };
    }
    if let Some(start) = location.line_start {
        let end = location.line_end.unwrap_or(start);
        return if start == end {
            format!("第 {start} 行")
        } else {
            format!("第 {start}–{end} 行")
        };
    }
    if let Some(paragraph) = location.paragraph_index {
        let paragraph = paragraph + 1;
        return match location.sentence_index {
            Some(sentence) => format!("第 {paragraph} 段，第 {} 句", sentence + 1),
            None => format!("第 {paragraph} 段"),
        };
    }
    format!("字符 {}–{}", location.char_start, location.char_end)
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
    fn cross_page_labels_and_legacy_reports_are_readable() {
        let legacy = serde_json::json!({
            "document_format": "pdf", "page": 1, "line_start": 50, "line_end": 3,
            "char_start": 0, "char_end": 2, "quote": "测试"
        });
        let mut location: SourceLocation = serde_json::from_value(legacy).unwrap();
        assert_eq!(location.page_end, None);
        assert_eq!(
            location_label(&location),
            "第 1 页第 50 行起（结束页未记录）"
        );
        location.page_end = Some(2);
        assert_eq!(location_label(&location), "第 1 页第 50 行至第 2 页第 3 行");
        let encoded = serde_json::to_value(&location).unwrap();
        assert_eq!(encoded["page_end"], 2);
        let report = ReportV1::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            DocumentMetadata {
                original_name: "测试.pdf".to_owned(),
                format: DocumentFormat::Pdf,
                char_count: 2,
            },
            AnalysisSnapshot {
                analysis_profile: AnalysisProfile::General,
                provider_kind: "openai_compatible".to_owned(),
                candidate_model: "test".to_owned(),
                verifier_model: "test".to_owned(),
                prompt_version: "test".to_owned(),
                reference_version: String::new(),
                analyzer_version: "test".to_owned(),
                reference_profile: false,
            },
            vec![Issue {
                id: Uuid::new_v4(),
                category: textcomb_domain::IssueCategory::Punctuation,
                grammar_subtype: None,
                level: IssueLevel::Suspected,
                location: location.clone(),
                original_text: "测试".to_owned(),
                reason: "测试原因".to_owned(),
                suggestion: "测试建议".to_owned(),
                confidence: 70,
                evidence_refs: vec![],
                feedback: None,
            }],
            OffsetDateTime::UNIX_EPOCH,
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["issues"][0]["location"]["page_end"],
            2
        );
        for export in [to_markdown(&report), to_typst(&report)] {
            assert!(export.contains("第 1 页第 50 行至第 2 页第 3 行"));
        }
        location.page_end = None;
        location.line_start = Some(2);
        assert_eq!(location_label(&location), "第 1 页，第 2–3 行");
    }

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
                reference_version: "sha256:test".to_owned(),
                analyzer_version: "v1".to_owned(),
                reference_profile: false,
            },
            vec![],
            OffsetDateTime::UNIX_EPOCH,
        );
        let markdown = to_markdown(&report);
        assert!(markdown.contains("未发现"));
        let pdf_source = to_typst(&report);
        for value in ["openai_compatible", "sha256:test", "未经验证", "v1"] {
            assert!(markdown.contains(value));
            assert!(pdf_source.contains(value));
        }
    }
}
