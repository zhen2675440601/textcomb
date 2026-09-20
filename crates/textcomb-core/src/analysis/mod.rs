use crate::{
    documents::ExtractedDocument,
    error::{CoreError, CoreResult, ErrorCode},
    provider::{CandidateIssue, VerificationVerdict, VerifiedCandidate},
};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};
use textcomb_domain::{EvidenceRef, GrammarSubtype, Issue, IssueCategory, IssueLevel};
use uuid::Uuid;

pub const TARGET_CHUNK_CHARS: usize = 3_000;
pub const HARD_CHUNK_CHARS: usize = 4_000;
pub const MAX_OVERLAP_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisChunk {
    pub index: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedCandidate {
    pub candidate_index: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub category: IssueCategory,
    pub grammar_subtype: Option<GrammarSubtype>,
    pub quote: String,
    pub reason: String,
    pub suggestion: String,
    pub candidate_confidence: u8,
    pub protected: bool,
}

pub fn chunk_text(text: &str) -> Vec<AnalysisChunk> {
    let paragraphs = paragraph_ranges(text);
    let mut chunks = Vec::new();
    let mut current_start = None;
    let mut current_end = 0_usize;
    let mut previous_paragraph: Option<(usize, usize)> = None;

    for (paragraph_start, paragraph_end) in paragraphs {
        let paragraph_len = paragraph_end.saturating_sub(paragraph_start);
        if paragraph_len > HARD_CHUNK_CHARS {
            if let Some(start) = current_start.take() {
                push_chunk(&mut chunks, text, start, current_end);
            }
            let mut cursor = paragraph_start;
            while cursor < paragraph_end {
                let end = split_long_paragraph(text, cursor, paragraph_end);
                push_chunk(&mut chunks, text, cursor, end);
                if end >= paragraph_end {
                    break;
                }
                cursor = end.saturating_sub(MAX_OVERLAP_CHARS).max(cursor + 1);
            }
            previous_paragraph = Some((paragraph_start, paragraph_end));
            current_end = paragraph_end;
            continue;
        }

        let proposed_start = current_start.unwrap_or(paragraph_start);
        let proposed_len = paragraph_end.saturating_sub(proposed_start);
        if current_start.is_some() && proposed_len > TARGET_CHUNK_CHARS {
            let start = current_start.take().expect("checked above");
            push_chunk(&mut chunks, text, start, current_end);
            let overlap_start = previous_paragraph
                .filter(|(start, end)| {
                    end.saturating_sub(*start) <= MAX_OVERLAP_CHARS
                        && paragraph_end.saturating_sub(*start) <= HARD_CHUNK_CHARS
                })
                .map(|(start, _)| start)
                .unwrap_or(paragraph_start);
            current_start = Some(overlap_start);
        } else if current_start.is_none() {
            current_start = Some(paragraph_start);
        }
        current_end = paragraph_end;
        previous_paragraph = Some((paragraph_start, paragraph_end));
    }

    if let Some(start) = current_start
        && current_end > start
    {
        push_chunk(&mut chunks, text, start, current_end);
    }
    for (index, chunk) in chunks.iter_mut().enumerate() {
        chunk.index = index;
    }
    chunks
}

fn split_long_paragraph(text: &str, start: usize, end: usize) -> usize {
    let hard_end = (start + HARD_CHUNK_CHARS).min(end);
    if hard_end == end {
        return end;
    }
    let window: Vec<char> = text.chars().skip(start).take(hard_end - start).collect();
    let search_start = window.len().saturating_sub(MAX_OVERLAP_CHARS);
    window
        .iter()
        .enumerate()
        .rev()
        .find(|(index, character)| {
            *index >= search_start
                && matches!(**character, '。' | '！' | '？' | '!' | '?' | '；' | ';')
        })
        .map_or(hard_end, |(index, _)| start + index + 1)
}

fn paragraph_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut cursor = 0_usize;
    while cursor < chars.len() {
        if chars[cursor] == '\n' && chars.get(cursor + 1) == Some(&'\n') {
            let end = cursor;
            if end > start {
                ranges.push((start, end));
            }
            cursor += 2;
            while cursor < chars.len() && chars[cursor] == '\n' {
                cursor += 1;
            }
            start = cursor;
        } else {
            cursor += 1;
        }
    }
    if start < chars.len() {
        ranges.push((start, chars.len()));
    }
    if ranges.is_empty() {
        ranges.push((0, chars.len()));
    }
    ranges
}

fn push_chunk(chunks: &mut Vec<AnalysisChunk>, text: &str, start: usize, end: usize) {
    let content: String = text.chars().skip(start).take(end - start).collect();
    if !content.trim().is_empty() {
        chunks.push(AnalysisChunk {
            index: chunks.len(),
            source_start: start,
            source_end: end,
            text: content,
        });
    }
}

pub fn resolve_candidates(
    chunk: &AnalysisChunk,
    candidates: Vec<CandidateIssue>,
) -> Vec<ResolvedCandidate> {
    candidates
        .into_iter()
        .enumerate()
        .filter_map(|(candidate_index, candidate)| {
            let local_start = locate_unique(
                &chunk.text,
                &candidate.quote,
                candidate.context_before.as_deref(),
                candidate.context_after.as_deref(),
            )?;
            let quote_len = candidate.quote.chars().count();
            let source_start = chunk.source_start + local_start;
            let source_end = source_start + quote_len;
            Some(ResolvedCandidate {
                candidate_index,
                source_start,
                source_end,
                category: candidate.category,
                grammar_subtype: candidate.grammar_subtype,
                quote: candidate.quote,
                reason: candidate.reason,
                suggestion: candidate.suggestion,
                candidate_confidence: candidate.confidence.min(100),
                protected: overlaps_protected_span(
                    &chunk.text,
                    local_start,
                    local_start + quote_len,
                ),
            })
        })
        .collect()
}

fn locate_unique(
    text: &str,
    quote: &str,
    context_before: Option<&str>,
    context_after: Option<&str>,
) -> Option<usize> {
    if quote.trim().is_empty() {
        return None;
    }
    let matches: Vec<usize> = text
        .match_indices(quote)
        .map(|(byte_index, _)| text[..byte_index].chars().count())
        .collect();
    if matches.len() == 1 {
        return matches.first().copied();
    }
    if matches.is_empty() {
        return None;
    }
    let text_chars: Vec<char> = text.chars().collect();
    let quote_len = quote.chars().count();
    let filtered: Vec<usize> = matches
        .into_iter()
        .filter(|start| {
            let before_ok = context_before.is_none_or(|context| {
                let context_chars: Vec<char> = context.chars().collect();
                let take = context_chars.len().min(*start);
                text_chars[start - take..*start]
                    .iter()
                    .collect::<String>()
                    .ends_with(context)
            });
            let after_start = start + quote_len;
            let after_ok = context_after.is_none_or(|context| {
                text_chars
                    .get(after_start..)
                    .unwrap_or_default()
                    .iter()
                    .collect::<String>()
                    .starts_with(context)
            });
            before_ok && after_ok
        })
        .collect();
    (filtered.len() == 1).then(|| filtered[0])
}

fn overlaps_protected_span(text: &str, start: usize, end: usize) -> bool {
    static PROTECTED: OnceLock<Regex> = OnceLock::new();
    let regex = PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?x)
            https?://[^\s]+ |
            www\.[^\s]+ |
            `[^`]+` |
            \$[^$]+\$ |
            [A-Za-z][A-Za-z0-9+.-]*://[^\s]+
            "#,
        )
        .expect("protected span regex is valid")
    });
    regex.find_iter(text).any(|found| {
        let match_start = text[..found.start()].chars().count();
        let match_end = match_start + found.as_str().chars().count();
        match_start < end && match_end > start
    })
}

pub fn finalize_issues(
    document: &ExtractedDocument,
    resolved: Vec<ResolvedCandidate>,
    verdicts: Vec<VerifiedCandidate>,
    evidence: &HashMap<String, EvidenceRef>,
    confirmed_threshold: u8,
    suspected_threshold: u8,
) -> CoreResult<Vec<Issue>> {
    let mut issues = Vec::new();
    for verdict in verdicts {
        let Some(candidate) = resolved
            .iter()
            .find(|candidate| candidate.candidate_index == verdict.candidate_index)
        else {
            continue;
        };
        let effective_confidence = candidate.candidate_confidence.min(verdict.confidence);
        if verdict.verdict == VerificationVerdict::Rejected
            || effective_confidence < suspected_threshold
        {
            continue;
        }
        let mut level = if effective_confidence >= confirmed_threshold
            && verdict.verdict == VerificationVerdict::Confirmed
        {
            IssueLevel::Confirmed
        } else {
            IssueLevel::Suspected
        };
        if candidate.category == IssueCategory::Paragraph
            || candidate.protected
            || requires_contextual_review(candidate)
        {
            level = IssueLevel::Suspected;
        }
        let mut seen_sources = HashSet::new();
        let evidence_refs = verdict
            .evidence_source_ids
            .iter()
            .filter_map(|source_id| {
                if seen_sources.insert(source_id.clone()) {
                    evidence.get(source_id).cloned()
                } else {
                    None
                }
            })
            .collect();
        let location = document.location(
            candidate.source_start,
            candidate.source_end,
            &candidate.quote,
        )?;
        issues.push(Issue {
            id: Uuid::new_v4(),
            category: candidate.category,
            grammar_subtype: candidate.grammar_subtype,
            level,
            location,
            original_text: candidate.quote.clone(),
            reason: if verdict.reason.trim().is_empty() {
                candidate.reason.clone()
            } else {
                verdict.reason
            },
            suggestion: if verdict.suggestion.trim().is_empty() {
                candidate.suggestion.clone()
            } else {
                verdict.suggestion
            },
            confidence: effective_confidence.min(100),
            evidence_refs,
            feedback: None,
        });
    }
    Ok(deduplicate_issues(issues))
}

/// These diagnoses often depend on intent or discourse outside the sentence.
/// Keep them reviewable without presenting an AI inference as a mandatory edit.
fn requires_contextual_review(candidate: &ResolvedCandidate) -> bool {
    candidate.category == IssueCategory::Grammar
        && matches!(
            candidate.grammar_subtype,
            Some(
                GrammarSubtype::Ambiguity | GrammarSubtype::Illogical | GrammarSubtype::WordMisuse
            )
        )
}

pub fn deduplicate_issues(mut issues: Vec<Issue>) -> Vec<Issue> {
    issues.sort_by_key(|issue| {
        (
            issue.location.char_start,
            issue.location.char_end,
            issue.category.as_str(),
            std::cmp::Reverse(issue.confidence),
        )
    });
    let mut merged: Vec<Issue> = Vec::new();
    for issue in issues {
        let duplicate = merged.iter().position(|existing| {
            existing.category == issue.category
                && existing.grammar_subtype == issue.grammar_subtype
                && normalize(&existing.suggestion) == normalize(&issue.suggestion)
                && existing.location.char_start < issue.location.char_end
                && issue.location.char_start < existing.location.char_end
        });
        if let Some(index) = duplicate {
            if issue.confidence > merged[index].confidence {
                merged[index] = issue;
            }
        } else {
            merged.push(issue);
        }
    }
    merged.sort_by_key(|issue| (issue.location.char_start, issue.location.char_end));
    merged
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<String>().to_lowercase()
}

pub fn require_non_empty_chunks(chunks: &[AnalysisChunk]) -> CoreResult<()> {
    if chunks.is_empty() {
        return Err(CoreError::public(
            ErrorCode::ExtractionFailed,
            "正文无法切分为可分析文本块",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_preserves_range_and_overlaps_only_at_boundaries() {
        let paragraph = "中".repeat(1_600);
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let chunks = chunk_text(&text);
        assert_eq!(chunks.len(), 3);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.text.chars().count() <= HARD_CHUNK_CHARS)
        );
        assert_eq!(chunks.first().unwrap().source_start, 0);
        assert_eq!(chunks.last().unwrap().source_end, text.chars().count());
    }

    #[test]
    fn long_paragraph_chunks_keep_hard_limit_and_context_overlap() {
        let text = format!("{}。{}", "中".repeat(4_200), "后文");
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.text.chars().count() <= HARD_CHUNK_CHARS)
        );
        assert!(
            chunks
                .windows(2)
                .all(|pair| pair[0].source_end > pair[1].source_start)
        );
        assert_eq!(chunks.last().unwrap().source_end, text.chars().count());
    }

    #[test]
    fn duplicate_quote_requires_context() {
        let text = "他说很好。她也说很好。";
        assert!(locate_unique(text, "很好", None, None).is_none());
        assert_eq!(locate_unique(text, "很好", Some("他说"), None), Some(2));
    }

    #[test]
    fn contextual_grammar_diagnoses_remain_suspected() {
        let document = ExtractedDocument {
            format: textcomb_domain::DocumentFormat::Txt,
            text: "这个人谁也不认识。".to_owned(),
            segments: vec![crate::documents::SourceSegment {
                char_start: 0,
                char_end: 9,
                page: None,
                line: Some(1),
                paragraph_index: 0,
            }],
            char_count: 9,
        };
        let candidate = ResolvedCandidate {
            candidate_index: 0,
            source_start: 0,
            source_end: 2,
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Ambiguity),
            quote: "这个".to_owned(),
            reason: "缺少上下文".to_owned(),
            suggestion: "补充指代对象".to_owned(),
            candidate_confidence: 100,
            protected: false,
        };
        let issues = finalize_issues(
            &document,
            vec![candidate],
            vec![VerifiedCandidate {
                candidate_index: 0,
                verdict: VerificationVerdict::Confirmed,
                confidence: 100,
                reason: "上下文不足以排除其他读法".to_owned(),
                suggestion: "补充指代对象".to_owned(),
                evidence_source_ids: Vec::new(),
            }],
            &HashMap::new(),
            80,
            50,
        )
        .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].level, IssueLevel::Suspected);
    }
}
