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
use textcomb_domain::{
    AnalysisProfile, EvidenceRef, GrammarSubtype, Issue, IssueCategory, IssueLevel,
};
use uuid::Uuid;

pub const TARGET_CHUNK_CHARS: usize = 3_000;
pub const HARD_CHUNK_CHARS: usize = 4_000;
pub const MAX_OVERLAP_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisChunk {
    pub index: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub core_start: usize,
    pub core_end: usize,
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
    let mut current_core_start = None;
    let mut current_core_end = 0_usize;
    let mut previous_paragraph: Option<(usize, usize)> = None;

    for (paragraph_start, paragraph_end) in paragraphs {
        let paragraph_len = paragraph_end.saturating_sub(paragraph_start);
        if paragraph_len > HARD_CHUNK_CHARS {
            if let Some(start) = current_start.take() {
                push_chunk(
                    &mut chunks,
                    text,
                    start,
                    current_end,
                    current_core_start.take().unwrap_or(start),
                    current_core_end,
                );
            }
            let mut cursor = paragraph_start;
            let mut core_start = paragraph_start;
            while cursor < paragraph_end {
                let end = split_long_paragraph(text, cursor, paragraph_end);
                let next_start = if end >= paragraph_end {
                    end
                } else {
                    end.saturating_sub(MAX_OVERLAP_CHARS).max(cursor + 1)
                };
                // Adjacent chunks share a context window. Give each character
                // in that window to exactly one chunk by splitting it in half.
                // The midpoint also lets a short quote crossing the ownership
                // boundary be accepted once by its midpoint.
                let core_end = if next_start < end {
                    next_start + (end - next_start) / 2
                } else {
                    end
                };
                push_chunk(&mut chunks, text, cursor, end, core_start, core_end);
                if end >= paragraph_end {
                    break;
                }
                cursor = next_start;
                core_start = core_end;
            }
            previous_paragraph = Some((paragraph_start, paragraph_end));
            current_end = paragraph_end;
            continue;
        }

        let proposed_start = current_start.unwrap_or(paragraph_start);
        let proposed_len = paragraph_end.saturating_sub(proposed_start);
        if current_start.is_some() && proposed_len > TARGET_CHUNK_CHARS {
            let start = current_start.take().expect("checked above");
            push_chunk(
                &mut chunks,
                text,
                start,
                current_end,
                current_core_start.take().unwrap_or(start),
                current_core_end,
            );
            let overlap_start = previous_paragraph
                .filter(|(start, end)| {
                    end.saturating_sub(*start) <= MAX_OVERLAP_CHARS
                        && paragraph_end.saturating_sub(*start) <= HARD_CHUNK_CHARS
                })
                .map(|(start, _)| start)
                .unwrap_or(paragraph_start);
            current_start = Some(overlap_start);
            current_core_start = Some(paragraph_start);
        } else if current_start.is_none() {
            current_start = Some(paragraph_start);
            current_core_start = Some(paragraph_start);
        }
        current_end = paragraph_end;
        current_core_end = paragraph_end;
        previous_paragraph = Some((paragraph_start, paragraph_end));
    }

    if let Some(start) = current_start
        && current_end > start
    {
        push_chunk(
            &mut chunks,
            text,
            start,
            current_end,
            current_core_start.unwrap_or(start),
            current_core_end,
        );
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

fn push_chunk(
    chunks: &mut Vec<AnalysisChunk>,
    text: &str,
    start: usize,
    end: usize,
    core_start: usize,
    core_end: usize,
) {
    let content: String = text.chars().skip(start).take(end - start).collect();
    if !content.trim().is_empty() {
        chunks.push(AnalysisChunk {
            index: chunks.len(),
            source_start: start,
            source_end: end,
            core_start: core_start.clamp(start, end),
            core_end: core_end.clamp(core_start.clamp(start, end), end),
            text: content,
        });
    }
}

pub fn resolve_candidates(
    chunk: &AnalysisChunk,
    candidates: Vec<CandidateIssue>,
    analysis_profile: AnalysisProfile,
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
            let source_midpoint = source_start + quote_len / 2;
            if source_midpoint < chunk.core_start || source_midpoint >= chunk.core_end {
                return None;
            }
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
                    analysis_profile,
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

fn overlaps_protected_span(
    text: &str,
    start: usize,
    end: usize,
    analysis_profile: AnalysisProfile,
) -> bool {
    static BASE_PROTECTED: OnceLock<Regex> = OnceLock::new();
    static ACADEMIC_PROTECTED: OnceLock<Regex> = OnceLock::new();
    static FINANCIAL_PROTECTED: OnceLock<Regex> = OnceLock::new();
    let base = BASE_PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?x)
            https?://[^\s]+ |
            www\.[^\s]+ |
            `[^`]+` |
            \$[^$]+\$ |
            [A-Za-z][A-Za-z0-9+.-]*://[^\s]+ |
            \\begin\{(?:equation|align\*?|math|displaymath)\}[\s\S]*?\\end\{(?:equation|align\*?|math|displaymath)\} |
            \\\([\s\S]*?\\\) |
            \\\[[\s\S]*?\\\]
        "#,
        )
        .expect("base protected span regex is valid")
    });
    let academic = ACADEMIC_PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?x)
            (?i:10\.\d{4,9}/[-._;()/:A-Z0-9]+) |
            \[\d{1,4}(?:\s*[-,，、]\s*\d{1,4})*\]
        "#,
        )
        .expect("academic protected span regex is valid")
    });
    let financial = FINANCIAL_PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?xi)
            [¥￥$€£]\s*\d[\d,，]*(?:\.\d+)? |
            \d[\d,，]*(?:\.\d+)?(?:%|‰|亿元|万元|人民币|美元|元|万|亿|股|人次) |
            \d{4}年(?:\d{1,2}月(?:\d{1,2}日)?)? |
            \d{6}\.(?:sh|sz|bj)
        "#,
        )
        .expect("financial protected span regex is valid")
    });
    regex_overlaps(base, text, start, end)
        || match analysis_profile {
            AnalysisProfile::General => false,
            AnalysisProfile::Academic => regex_overlaps(academic, text, start, end),
            AnalysisProfile::Financial => regex_overlaps(financial, text, start, end),
        }
}

fn regex_overlaps(regex: &Regex, text: &str, start: usize, end: usize) -> bool {
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
        assert_eq!(chunks.first().unwrap().core_start, 0);
        assert_eq!(chunks.last().unwrap().core_end, text.chars().count());
        assert!(chunks.windows(2).all(|pair| {
            pair[0].core_end <= pair[1].core_start
                && pair[0].core_start < pair[0].core_end
                && pair[1].core_start < pair[1].core_end
        }));
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
        assert!(
            chunks
                .windows(2)
                .all(|pair| pair[0].core_end == pair[1].core_start)
        );
        assert_eq!(chunks.first().unwrap().core_start, 0);
        assert_eq!(chunks.last().unwrap().core_end, text.chars().count());
        assert_eq!(chunks.last().unwrap().source_end, text.chars().count());
    }

    #[test]
    fn long_paragraph_boundary_quote_is_owned_once() {
        let quote = "唯一边界片段";
        let text = format!("{}{}{}", "甲".repeat(3_748), quote, "乙".repeat(5_000));
        let chunks = chunk_text(&text);
        let candidate = CandidateIssue {
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            quote: quote.to_owned(),
            context_before: None,
            context_after: None,
            reason: "测试".to_owned(),
            suggestion: "测试".to_owned(),
            confidence: 90,
            evidence_source_ids: Vec::new(),
        };
        let owned = chunks
            .iter()
            .map(|chunk| {
                resolve_candidates(chunk, vec![candidate.clone()], AnalysisProfile::General).len()
            })
            .sum::<usize>();
        assert_eq!(owned, 1);
    }

    #[test]
    fn duplicate_quote_requires_context() {
        let text = "他说很好。她也说很好。";
        assert!(locate_unique(text, "很好", None, None).is_none());
        assert_eq!(locate_unique(text, "很好", Some("他说"), None), Some(2));
    }

    #[test]
    fn academic_and_financial_fragments_are_protected_for_review() {
        let text = r#"公式 \(x^2 + y^2\)，引用[12-14]，DOI 10.1234/example.1，增长率 12.5%。"#;
        let range = |quote: &str| {
            let start = locate_unique(text, quote, None, None).expect("quote is unique");
            (start, start + quote.chars().count())
        };
        let (formula_start, formula_end) = range("x^2 + y^2");
        assert!(overlaps_protected_span(
            text,
            formula_start,
            formula_end,
            AnalysisProfile::General
        ));
        for quote in ["12-14", "10.1234/example.1"] {
            let (start, end) = range(quote);
            assert!(overlaps_protected_span(
                text,
                start,
                end,
                AnalysisProfile::Academic
            ));
            assert!(!overlaps_protected_span(
                text,
                start,
                end,
                AnalysisProfile::General
            ));
        }
        let (percentage_start, percentage_end) = range("12.5%");
        assert!(overlaps_protected_span(
            text,
            percentage_start,
            percentage_end,
            AnalysisProfile::Financial
        ));
        assert!(!overlaps_protected_span(
            text,
            percentage_start,
            percentage_end,
            AnalysisProfile::General
        ));
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
