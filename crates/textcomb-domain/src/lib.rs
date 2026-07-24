use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use time::OffsetDateTime;
use uuid::Uuid;

pub const REPORT_SCHEMA_V1: &str = "textcomb.report.v1";
pub const ANALYZER_VERSION: &str = env!("CARGO_PKG_VERSION");

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    _ => Err(ParseEnumError {
                        enum_name: stringify!($name),
                        value: value.to_owned(),
                    }),
                }
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseEnumError {
    pub enum_name: &'static str,
    pub value: String,
}

impl fmt::Display for ParseEnumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {} value: {}", self.enum_name, self.value)
    }
}

impl std::error::Error for ParseEnumError {}

string_enum!(AnalysisStatus {
    Queued => "queued",
    Extracting => "extracting",
    Analyzing => "analyzing",
    Verifying => "verifying",
    Merging => "merging",
    Rendering => "rendering",
    Completed => "completed",
    Failed => "failed",
    CancelRequested => "cancel_requested",
    Cancelled => "cancelled",
    Expired => "expired",
});

string_enum!(IssueCategory {
    Typo => "typo",
    Punctuation => "punctuation",
    Grammar => "grammar",
    Paragraph => "paragraph",
});

string_enum!(GrammarSubtype {
    WordOrder => "word_order",
    Collocation => "collocation",
    MissingOrRedundantComponent => "missing_or_redundant_component",
    MixedStructure => "mixed_structure",
    Ambiguity => "ambiguity",
    Illogical => "illogical",
    Conjunction => "conjunction",
    WordMisuse => "word_misuse",
});

string_enum!(IssueLevel {
    Confirmed => "confirmed",
    Suspected => "suspected",
});

string_enum!(FeedbackVerdict {
    Correct => "correct",
    Incorrect => "incorrect",
    Disputed => "disputed",
});

string_enum!(DocumentFormat {
    Txt => "txt",
    Docx => "docx",
    Pdf => "pdf",
});

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    pub source_id: String,
    pub title: String,
    pub revision: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceLocation {
    pub document_format: DocumentFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentence_index: Option<u32>,
    pub char_start: u32,
    pub char_end: u32,
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub id: Uuid,
    pub category: IssueCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar_subtype: Option<GrammarSubtype>,
    pub level: IssueLevel,
    pub location: SourceLocation,
    pub original_text: String,
    pub reason: String,
    pub suggestion: String,
    pub confidence: u8,
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<FeedbackVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentMetadata {
    pub original_name: String,
    pub format: DocumentFormat,
    pub char_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalysisSnapshot {
    pub provider_kind: String,
    pub candidate_model: String,
    pub verifier_model: String,
    pub prompt_version: String,
    pub analyzer_version: String,
    pub reference_profile: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReportSummary {
    pub total: u32,
    pub confirmed: u32,
    pub suspected: u32,
    pub typo: u32,
    pub punctuation: u32,
    pub grammar: u32,
    pub paragraph: u32,
}

impl ReportSummary {
    pub fn from_issues(issues: &[Issue]) -> Self {
        let mut summary = Self::default();
        for issue in issues {
            summary.total += 1;
            match issue.level {
                IssueLevel::Confirmed => summary.confirmed += 1,
                IssueLevel::Suspected => summary.suspected += 1,
            }
            match issue.category {
                IssueCategory::Typo => summary.typo += 1,
                IssueCategory::Punctuation => summary.punctuation += 1,
                IssueCategory::Grammar => summary.grammar += 1,
                IssueCategory::Paragraph => summary.paragraph += 1,
            }
        }
        summary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportV1 {
    pub schema: String,
    pub report_id: Uuid,
    pub job_id: Uuid,
    pub document: DocumentMetadata,
    pub analysis: AnalysisSnapshot,
    pub summary: ReportSummary,
    pub issues: Vec<Issue>,
    pub complete: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
}

impl ReportV1 {
    pub fn new(
        report_id: Uuid,
        job_id: Uuid,
        document: DocumentMetadata,
        analysis: AnalysisSnapshot,
        issues: Vec<Issue>,
        generated_at: OffsetDateTime,
    ) -> Self {
        let summary = ReportSummary::from_issues(&issues);
        Self {
            schema: REPORT_SCHEMA_V1.to_owned(),
            report_id,
            job_id,
            document,
            analysis,
            summary,
            issues,
            complete: true,
            generated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_each_dimension() {
        let location = SourceLocation {
            document_format: DocumentFormat::Txt,
            page: None,
            line_start: Some(1),
            line_end: Some(1),
            paragraph_index: Some(0),
            sentence_index: Some(0),
            char_start: 0,
            char_end: 2,
            quote: "测试".to_owned(),
        };
        let issues = vec![
            Issue {
                id: Uuid::nil(),
                category: IssueCategory::Grammar,
                grammar_subtype: Some(GrammarSubtype::Collocation),
                level: IssueLevel::Confirmed,
                location: location.clone(),
                original_text: "测试".to_owned(),
                reason: "原因".to_owned(),
                suggestion: "建议".to_owned(),
                confidence: 91,
                evidence_refs: vec![],
                feedback: None,
            },
            Issue {
                id: Uuid::nil(),
                category: IssueCategory::Paragraph,
                grammar_subtype: None,
                level: IssueLevel::Suspected,
                location,
                original_text: "测试".to_owned(),
                reason: "原因".to_owned(),
                suggestion: "建议".to_owned(),
                confidence: 70,
                evidence_refs: vec![],
                feedback: None,
            },
        ];
        let summary = ReportSummary::from_issues(&issues);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.confirmed, 1);
        assert_eq!(summary.suspected, 1);
        assert_eq!(summary.grammar, 1);
        assert_eq!(summary.paragraph, 1);
    }
}
