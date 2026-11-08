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
                    candidate.category,
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
    // `match_indices` skips overlapping occurrences (for example, 哈哈 in 哈哈哈).
    // Every character boundary must be checked before a quote can be called unique.
    let matches: Vec<usize> = text
        .char_indices()
        .filter(|(byte_index, _)| text[*byte_index..].starts_with(quote))
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
    category: IssueCategory,
) -> bool {
    static BASE_PROTECTED: OnceLock<Regex> = OnceLock::new();
    static ACADEMIC_PROTECTED: OnceLock<Regex> = OnceLock::new();
    static FINANCIAL_PROTECTED: OnceLock<Regex> = OnceLock::new();
    let base = BASE_PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?x)
            (?:[A-Za-z][A-Za-z0-9+.-]*://|www\.)[^\s<>"'`，。！？；：、（）【】［］《》“”‘’]+ |
            `[^`]+` |
            \\begin\{(?:equation|align\*?|math|displaymath)\}[\s\S]*?\\end\{(?:equation|align\*?|math|displaymath)\} |
            \\\([\s\S]*?\\\) |
            \\\[[\s\S]*?\\\]
        "#,
        )
        .expect("base protected span regex is valid")
    });
    let academic = ACADEMIC_PROTECTED.get_or_init(|| {
        let author_year =
            r"[\p{Han}A-Za-z][\p{Han}A-Za-z\x20.·&、-]{0,60}[,，][\t\x20]*(?:19|20)\d{2}[a-z]?";
        let numeric_and_doi = r"(?x)
            (?i:10\.\d{4,9}/[-._;()/:A-Z0-9]+) |
            \[\d{1,4}(?:[\t\x20]*[-–—,，、][\t\x20]*\d{1,4})*\] |
            ［\d{1,4}(?:[\t\x20]*[-–—,，、][\t\x20]*\d{1,4})*］";
        Regex::new(&format!(
            r"{numeric_and_doi} | \({author_year}\) | （{author_year}）"
        ))
        .expect("academic protected span regex is valid")
    });
    let financial = FINANCIAL_PROTECTED.get_or_init(|| {
        Regex::new(
            r#"(?xi)
            [¥￥$€£][\t\x20]*\d+(?:[,，]\d{3})*(?:\.\d+)? |
            \d+(?:[,，]\d{3})*(?:\.\d+)?(?:%|％|‰|亿元|万元|人民币|美元|元|万|亿|股|人次) |
            \d{4}年(?:\d{1,2}月(?:\d{1,2}日)?)? |
            (?-u:\b)[0-9]{4}-[0-9]{2}-[0-9]{2}(?-u:\b) |
            \d{6}\.(?:sh|sz|bj)
        "#,
        )
        .expect("financial protected span regex is valid")
    });
    let matches = |byte_start, byte_end| {
        let match_start = text[..byte_start].chars().count();
        let match_end = match_start + text[byte_start..byte_end].chars().count();
        if category == IssueCategory::Grammar {
            match_start <= start && match_end >= end
        } else {
            match_start < end && match_end > start
        }
    };
    let matches_regex = |regex: &Regex| {
        regex
            .find_iter(text)
            .any(|found| matches(found.start(), found.end()))
    };
    matches_regex(base)
        || any_dollar_math_span(text, matches)
        || match analysis_profile {
            AnalysisProfile::General => false,
            AnalysisProfile::Academic => matches_regex(academic),
            AnalysisProfile::Financial => matches_regex(financial),
        }
}

fn any_dollar_math_span(text: &str, matches: impl Fn(usize, usize) -> bool) -> bool {
    static AMOUNT_FOLLOWED_BY_PROSE: OnceLock<Regex> = OnceLock::new();
    let amount_followed_by_prose = AMOUNT_FOLLOWED_BY_PROSE.get_or_init(|| {
        Regex::new(r"^\d[\d.,]*[ \t]*\p{Han}").expect("amount followed by prose regex is valid")
    });
    let mut delimiters = text
        .match_indices('$')
        .filter_map(|(index, _)| {
            let escapes = text[..index]
                .bytes()
                .rev()
                .take_while(|&b| b == b'\\')
                .count();
            (escapes % 2 == 0).then_some(index)
        })
        .peekable();
    let mut opening = None;
    while let Some(index) = delimiters.next() {
        let width = if delimiters.peek() == Some(&(index + 1)) {
            delimiters.next();
            2
        } else {
            1
        };
        if let Some((begin, opening_width)) = opening
            && width == opening_width
        {
            let body = &text[begin + opening_width..index];
            // Currency symbols must not pair across prose. Inline math uses
            // adjacent non-space delimiters, and a closing dollar cannot prefix
            // another amount. An invalid pair must not consume a later formula.
            let valid_body = !body.trim().is_empty()
                && (width == 2
                    || (body.trim() == body
                        && !body.contains(['\r', '\n', '`', '，', '。', '！', '？', '；'])
                        && !amount_followed_by_prose.is_match(body)));
            let prefixes_amount = width == 1
                && text[index + width..]
                    .starts_with(|c: char| c.is_ascii_digit() || matches!(c, '０'..='９'));
            if valid_body && !prefixes_amount {
                if matches(begin, index + width) {
                    return true;
                }
                opening = None;
                continue;
            }
        }
        opening = Some((index, width));
    }
    false
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
        if verdict.verdict == VerificationVerdict::Rejected
            || verdict.confidence < suspected_threshold
        {
            continue;
        }
        let category = verdict.category.unwrap_or(candidate.category);
        let grammar_subtype = if verdict.category.is_some() {
            verdict.grammar_subtype
        } else {
            candidate.grammar_subtype
        };
        let classification_changed = verdict.category.is_some()
            && (category != candidate.category
                || (category == IssueCategory::Grammar
                    && grammar_subtype != candidate.grammar_subtype));
        let mut level = if candidate.candidate_confidence >= confirmed_threshold
            && verdict.confidence >= confirmed_threshold
            && verdict.verdict == VerificationVerdict::Confirmed
        {
            IssueLevel::Confirmed
        } else {
            IssueLevel::Suspected
        };
        if classification_changed
            || category == IssueCategory::Paragraph
            || candidate.protected
            || requires_contextual_review(category, grammar_subtype)
        {
            level = IssueLevel::Suspected;
        }
        let mut seen_sources = HashSet::new();
        let evidence_refs = verdict
            .evidence_source_ids
            .iter()
            .filter_map(|source_id| {
                if seen_sources.insert(source_id.clone())
                    && evidence_allowed_for_category(source_id, category)
                {
                    evidence.get(source_id).cloned()
                } else {
                    None
                }
            })
            .collect();
        // Refine a resolved source span only after verification. Keep the level
        // derived from the original protected span and classification agreement.
        let edit = (!classification_changed)
            .then(|| localize_literal_edit(category, &candidate.quote, &verdict.suggestion))
            .flatten();
        let (source_start, source_end, original_text, suggestion) = match edit {
            Some(edit) => {
                let start = candidate.source_start + edit.char_offset;
                let end = start + edit.original_text.chars().count();
                (start, end, edit.original_text, edit.suggestion)
            }
            None => (
                candidate.source_start,
                candidate.source_end,
                candidate.quote.clone(),
                verdict.suggestion,
            ),
        };
        let location = document.location(source_start, source_end, &original_text)?;
        issues.push(Issue {
            id: Uuid::new_v4(),
            category,
            grammar_subtype,
            level,
            location,
            original_text,
            reason: verdict.reason,
            suggestion,
            confidence: verdict.confidence.min(100),
            evidence_refs,
            feedback: None,
        });
    }
    Ok(deduplicate_issues(issues))
}

struct LiteralEdit {
    char_offset: usize,
    original_text: String,
    suggestion: String,
}

fn localize_literal_edit(
    category: IssueCategory,
    quote: &str,
    suggestion: &str,
) -> Option<LiteralEdit> {
    if !matches!(category, IssueCategory::Typo | IssueCategory::Punctuation) {
        return None;
    }
    let original: Vec<char> = quote.chars().collect();
    let replacement: Vec<char> = suggestion.chars().collect();
    let prefix = original
        .iter()
        .zip(&replacement)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = original[prefix..]
        .iter()
        .rev()
        .zip(replacement[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let shared = prefix + suffix;
    let original_end = original.len() - suffix;
    let replacement_end = replacement.len() - suffix;
    // This is literal edit alignment, not a language rule or an interpretation
    // of free-form advice. Most of the quoted context must be preserved, the
    // change must be small, and insertions retain a nonempty contextual quote.
    if shared < 4
        || shared * 4 < original.len() * 3
        || original_end == prefix
        || original_end - prefix > 8
        || replacement_end - prefix > 8
    {
        return None;
    }
    let original_text: String = original[prefix..original_end].iter().collect();
    let replacement_text: String = replacement[prefix..replacement_end].iter().collect();
    let suggestion = if replacement_text.is_empty() {
        format!("删除此处的“{original_text}”")
    } else {
        replacement_text
    };
    Some(LiteralEdit {
        char_offset: prefix,
        original_text,
        suggestion,
    })
}

/// These diagnoses often depend on intent or discourse outside the sentence.
/// Keep them reviewable without presenting an AI inference as a mandatory edit.
fn requires_contextual_review(
    category: IssueCategory,
    grammar_subtype: Option<GrammarSubtype>,
) -> bool {
    category == IssueCategory::Grammar
        && matches!(
            grammar_subtype,
            Some(
                GrammarSubtype::Ambiguity | GrammarSubtype::Illogical | GrammarSubtype::WordMisuse
            )
        )
}

fn evidence_allowed_for_category(source_id: &str, category: IssueCategory) -> bool {
    matches!(
        (source_id, category),
        ("gb-t-15834-2011", IssueCategory::Punctuation)
            | ("standard-chinese-characters-2013", IssueCategory::Typo)
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
            AnalysisProfile::General,
            IssueCategory::Typo
        ));
        for quote in ["12-14", "10.1234/example.1"] {
            let (start, end) = range(quote);
            assert!(overlaps_protected_span(
                text,
                start,
                end,
                AnalysisProfile::Academic,
                IssueCategory::Typo
            ));
            assert!(!overlaps_protected_span(
                text,
                start,
                end,
                AnalysisProfile::General,
                IssueCategory::Typo
            ));
        }
        let (percentage_start, percentage_end) = range("12.5%");
        assert!(overlaps_protected_span(
            text,
            percentage_start,
            percentage_end,
            AnalysisProfile::Financial,
            IssueCategory::Typo
        ));
        assert!(!overlaps_protected_span(
            text,
            percentage_start,
            percentage_end,
            AnalysisProfile::General,
            IssueCategory::Typo
        ));
        let sentence = "2025年，公司的经营状况得到了提高。";
        assert!(!overlaps_protected_span(
            sentence,
            0,
            sentence.chars().count(),
            AnalysisProfile::Financial,
            IssueCategory::Grammar
        ));
        assert!(overlaps_protected_span(
            sentence,
            0,
            5,
            AnalysisProfile::Financial,
            IssueCategory::Typo
        ));
    }

    #[test]
    fn adjacent_currency_and_urls_do_not_downgrade_verified_prose() {
        for text in [
            "预算为$100，准备工作非常充份，实际支出为$200。",
            "预算为$100准备工作非常充份实际支出为$200。",
            "预算为$100准备工作非常充份公式$x+y$。",
            "预算为$100\n准备工作非常充份\n实际支出为$200。",
            "详情见https://example.com，准备工作非常充份。",
            "详情见www.example.com。准备工作非常充份。",
            "详情见ftp://example.com/file；准备工作非常充份。",
            "详情见“https://example.com/中文路径”，准备工作非常充份。",
        ] {
            for profile in [
                AnalysisProfile::General,
                AnalysisProfile::Academic,
                AnalysisProfile::Financial,
            ] {
                let (document, resolved, verdicts) = literal_fixture(
                    text,
                    "工作非常充份",
                    &["工作非常充分"],
                    IssueCategory::Typo,
                    profile,
                );
                assert!(!resolved[0].protected, "{profile:?}: {text}");
                let issues =
                    finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
                        .expect("verified prose beside a protected fragment");
                assert_eq!(
                    issues[0].level,
                    IssueLevel::Confirmed,
                    "{profile:?}: {text}"
                );
                assert_eq!(issues[0].original_text, "份");
                let offset = text[..text.find('份').expect("typo")].chars().count() as u32;
                assert_eq!(issues[0].location.char_start, offset);
                assert_eq!(issues[0].location.char_end, offset + 1);
            }
        }
    }

    #[test]
    fn dollar_math_and_url_contents_remain_protected() {
        for (text, quote) in [
            ("公式$x^2 + y^2$，后面是正文。", "x^2 + y^2"),
            ("常数$100$。", "100"),
            ("预算$100，另有公式$x+y$。", "x+y"),
            ("预算$100另有公式$x+y$。", "x+y"),
            ("预算$100 and $200 then $x+y$。", "x+y"),
            ("公式$$\nx+y\n$$。", "x+y"),
            (r"公式$\text{价格}\$100 + x$。", r"\text{价格}\$100 + x"),
            (
                "网址https://example.com/中文路径?q=1&b=2#part，正文。",
                "中文路径",
            ),
            ("网址www.example.com/a，正文。", "example"),
            ("地址ftp://example.com/a，正文。", "example"),
            ("代码`value = 100`，正文。", "value = 100"),
        ] {
            let start = locate_unique(text, quote, None, None).expect("unique fragment");
            for category in [
                IssueCategory::Typo,
                IssueCategory::Punctuation,
                IssueCategory::Grammar,
            ] {
                assert!(
                    overlaps_protected_span(
                        text,
                        start,
                        start + quote.chars().count(),
                        AnalysisProfile::General,
                        category
                    ),
                    "{category:?}: {text}"
                );
            }
        }
        for text in [
            r"预算\$100，支出\$200。",
            "价格$100至$200。",
            "价格$１００至$２００。",
            "$没有闭合",
        ] {
            assert!(!any_dollar_math_span(text, |_, _| true), "{text}");
        }
    }

    #[test]
    fn protected_amount_excludes_following_chinese_punctuation() {
        let text = "预算为$1，234.50，，项目已启动。";
        for (quote, expected) in [("$1，234.50", true), ("，，", false)] {
            let start = locate_unique(text, quote, None, None).expect("unique fragment");
            assert_eq!(
                overlaps_protected_span(
                    text,
                    start,
                    start + quote.chars().count(),
                    AnalysisProfile::Financial,
                    IssueCategory::Punctuation
                ),
                expected,
            );
        }
    }

    #[test]
    fn scene_variants_are_suspected_only_in_their_own_profile() {
        for (text, quote, protected_profile) in [
            (
                "🧪参见［12］，准备充份。",
                "［12］",
                AnalysisProfile::Academic,
            ),
            (
                "参见［12–14， 16］。",
                "［12–14， 16］",
                AnalysisProfile::Academic,
            ),
            (
                "已有研究（张某，2024）提出观点。",
                "（张某，2024）",
                AnalysisProfile::Academic,
            ),
            (
                "已有研究(Smith et al., 2024a)提出观点。",
                "(Smith et al., 2024a)",
                AnalysisProfile::Academic,
            ),
            (
                "截至2026-10-22，收入增长。",
                "2026-10-22",
                AnalysisProfile::Financial,
            ),
            (
                "增长率为12.5％，收入增长。",
                "12.5％",
                AnalysisProfile::Financial,
            ),
        ] {
            for profile in [
                AnalysisProfile::General,
                AnalysisProfile::Academic,
                AnalysisProfile::Financial,
            ] {
                for category in [
                    IssueCategory::Typo,
                    IssueCategory::Punctuation,
                    IssueCategory::Grammar,
                ] {
                    let (document, resolved, verdicts) =
                        literal_fixture(text, quote, &["请人工复核此片段"], category, profile);
                    let expected = if profile == protected_profile {
                        IssueLevel::Suspected
                    } else {
                        IssueLevel::Confirmed
                    };
                    let issues =
                        finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
                            .expect("scene fragment report");
                    assert_eq!(
                        issues[0].level, expected,
                        "{profile:?}/{category:?}: {text}"
                    );
                    assert_eq!(issues[0].location.quote, quote);
                }
            }
            assert!(!overlaps_protected_span(
                text,
                0,
                text.chars().count(),
                protected_profile,
                IssueCategory::Grammar
            ));
        }
    }

    #[test]
    fn malformed_citation_brackets_do_not_protect_punctuation() {
        let text = "已有研究（张某，2024)提出观点。";
        let start = locate_unique(text, ")", None, None).expect("unique bracket");
        assert!(!overlaps_protected_span(
            text,
            start,
            start + 1,
            AnalysisProfile::Academic,
            IssueCategory::Punctuation,
        ));
    }

    #[test]
    fn overlapping_quotes_are_ambiguous_without_context() {
        assert_eq!(locate_unique("哈哈哈", "哈哈", None, None), None);
        assert_eq!(locate_unique("哈哈哈", "哈哈", None, Some("哈")), Some(0));
    }

    fn literal_fixture(
        text: &str,
        quote: &str,
        suggestions: &[&str],
        category: IssueCategory,
        profile: AnalysisProfile,
    ) -> (
        ExtractedDocument,
        Vec<ResolvedCandidate>,
        Vec<VerifiedCandidate>,
    ) {
        let char_count = text.chars().count();
        let document = ExtractedDocument {
            format: textcomb_domain::DocumentFormat::Txt,
            text: text.to_owned(),
            segments: vec![crate::documents::SourceSegment {
                char_start: 0,
                char_end: char_count,
                page: None,
                line: Some(1),
                paragraph_index: 0,
            }],
            char_count,
        };
        let subtype = (category == IssueCategory::Grammar).then_some(GrammarSubtype::Collocation);
        let candidates = suggestions
            .iter()
            .map(|suggestion| CandidateIssue {
                category,
                grammar_subtype: subtype,
                quote: quote.to_owned(),
                context_before: None,
                context_after: None,
                reason: "Synthetic literal edit".to_owned(),
                suggestion: (*suggestion).to_owned(),
                confidence: 95,
                evidence_source_ids: Vec::new(),
            })
            .collect();
        let chunks = chunk_text(text);
        assert_eq!(chunks.len(), 1);
        let resolved = resolve_candidates(&chunks[0], candidates, profile);
        assert_eq!(resolved.len(), suggestions.len());
        let verdicts = suggestions
            .iter()
            .enumerate()
            .map(|(index, suggestion)| VerifiedCandidate {
                candidate_index: index,
                category: Some(category),
                grammar_subtype: subtype,
                verdict: VerificationVerdict::Confirmed,
                confidence: 95,
                reason: "Independent synthetic verification".to_owned(),
                suggestion: (*suggestion).to_owned(),
                evidence_source_ids: Vec::new(),
            })
            .collect();
        (document, resolved, verdicts)
    }

    #[test]
    fn literal_edits_keep_unicode_offsets_and_resolved_duplicate_context() {
        let text = "再来一次。🧪我们再教室里学习。再见。";
        let quote = "🧪我们再教室里学习。";
        let (document, resolved, verdicts) = literal_fixture(
            text,
            quote,
            &["🧪我们在教室里学习。"],
            IssueCategory::Typo,
            AnalysisProfile::General,
        );
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("verified literal edit");
        let issue = &issues[0];
        assert_eq!(issue.original_text, "再");
        assert_eq!(issue.suggestion, "在");
        assert_eq!(issue.location.char_start, 8);
        assert_eq!(issue.location.char_end, 9);
        assert_eq!(issue.location.quote, "再");
        assert_eq!(document.text, text);
        assert_eq!(issue.level, IssueLevel::Confirmed);
    }

    #[test]
    fn literal_punctuation_deletion_has_a_nonempty_readable_suggestion() {
        let quote = "大家准时到达了。。";
        let (document, resolved, verdicts) = literal_fixture(
            quote,
            quote,
            &["大家准时到达了。"],
            IssueCategory::Punctuation,
            AnalysisProfile::General,
        );
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("verified deletion");
        assert_eq!(issues[0].original_text, "。");
        assert_eq!(issues[0].suggestion, "删除此处的“。”");
        assert_eq!(issues[0].location.char_start, 8);
        assert_eq!(issues[0].location.char_end, 9);
    }

    #[test]
    fn literal_edits_preserve_the_original_protected_span_downgrade() {
        let quote = "2026年，我们再教室里学习。";
        let (document, resolved, verdicts) = literal_fixture(
            quote,
            quote,
            &["2026年，我们在教室里学习。"],
            IssueCategory::Typo,
            AnalysisProfile::Financial,
        );
        assert!(resolved[0].protected);
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("protected literal edit");
        assert_eq!(issues[0].original_text, "再");
        assert_eq!(issues[0].level, IssueLevel::Suspected);
    }

    #[test]
    fn literal_edits_preserve_grammar_context_and_classification_disagreements() {
        let quote = "他的心情很繁重。";
        let (document, resolved, verdicts) = literal_fixture(
            quote,
            quote,
            &["他的心情很沉重。"],
            IssueCategory::Grammar,
            AnalysisProfile::General,
        );
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("grammar context");
        assert_eq!(issues[0].original_text, quote);
        let quote = "我们再教室里学习。";
        let (document, mut resolved, verdicts) = literal_fixture(
            quote,
            quote,
            &["我们在教室里学习。"],
            IssueCategory::Typo,
            AnalysisProfile::General,
        );
        resolved[0].category = IssueCategory::Grammar;
        resolved[0].grammar_subtype = Some(GrammarSubtype::Collocation);
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("classification disagreement");
        assert_eq!(issues[0].original_text, quote);
        assert_eq!(issues[0].level, IssueLevel::Suspected);
    }

    #[test]
    fn literal_alignment_leaves_advice_insertions_and_large_changes_unchanged() {
        for (quote, suggestion) in [
            ("我们再教室里学习。", "建议将再改成在"),
            ("今天大家到了。", "今天大家都到了。"),
            ("我们再教室里学习。", "我们再教室里学习。"),
            ("我们再教室里学习。", ""),
            ("错", "正"),
            ("", ""),
            (
                "前文前文前文前文前文前文前文前文前文错误片段超过八个字后文后文后文后文后文后文",
                "前文前文前文前文前文前文前文前文前文正确后文后文后文后文后文后文",
            ),
        ] {
            assert!(
                localize_literal_edit(IssueCategory::Typo, quote, suggestion).is_none(),
                "{quote}"
            );
        }
    }

    #[test]
    fn literal_alignment_keeps_distinct_edits_from_the_same_sentence() {
        let quote = "今天我们再教室里学习，所有资料已经全步准备齐全。";
        let first = quote.replace('再', "在");
        let second = quote.replace('步', "部");
        let (document, resolved, verdicts) = literal_fixture(
            quote,
            quote,
            &[&first, &second],
            IssueCategory::Typo,
            AnalysisProfile::General,
        );
        let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
            .expect("two independently verified edits");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].original_text, "再");
        assert_eq!(issues[1].original_text, "步");
        for issue in &issues {
            let source: String = document
                .text
                .chars()
                .skip(issue.location.char_start as usize)
                .take((issue.location.char_end - issue.location.char_start) as usize)
                .collect();
            assert_eq!(source, issue.original_text);
        }
    }

    #[test]
    fn literal_alignment_cannot_rescue_rejected_or_low_confidence_predictions() {
        for (verdict, confidence) in [
            (VerificationVerdict::Rejected, 95),
            (VerificationVerdict::Confirmed, 49),
        ] {
            let quote = "我们再教室里学习。";
            let (document, resolved, mut verdicts) = literal_fixture(
                quote,
                quote,
                &["我们在教室里学习。"],
                IssueCategory::Typo,
                AnalysisProfile::General,
            );
            verdicts[0].verdict = verdict;
            verdicts[0].confidence = confidence;
            let issues = finalize_issues(&document, resolved, verdicts, &HashMap::new(), 80, 50)
                .expect("rejected prediction");
            assert!(issues.is_empty());
        }
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
                category: None,
                grammar_subtype: None,
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

    #[test]
    fn reviewer_can_rescue_low_scored_candidate_only_as_suspected() {
        let document = ExtractedDocument {
            format: textcomb_domain::DocumentFormat::Txt,
            text: "他心情很繁重。".to_owned(),
            segments: vec![crate::documents::SourceSegment {
                char_start: 0,
                char_end: 7,
                page: None,
                line: Some(1),
                paragraph_index: 0,
            }],
            char_count: 7,
        };
        let candidate = ResolvedCandidate {
            candidate_index: 0,
            source_start: 1,
            source_end: 6,
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            quote: "心情很繁重".to_owned(),
            reason: "初检理由".to_owned(),
            suggestion: "初检建议".to_owned(),
            candidate_confidence: 30,
            protected: false,
        };
        let reviewed = VerifiedCandidate {
            candidate_index: 0,
            category: Some(IssueCategory::Grammar),
            grammar_subtype: Some(GrammarSubtype::WordOrder),
            verdict: VerificationVerdict::Confirmed,
            confidence: 90,
            reason: "心情与繁重搭配不当".to_owned(),
            suggestion: "心情很沉重".to_owned(),
            evidence_source_ids: Vec::new(),
        };
        let issues = finalize_issues(
            &document,
            vec![candidate],
            vec![reviewed],
            &HashMap::new(),
            80,
            50,
        )
        .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].level, IssueLevel::Suspected);
        assert_eq!(issues[0].confidence, 90);
        assert_eq!(issues[0].grammar_subtype, Some(GrammarSubtype::WordOrder));
    }

    #[test]
    fn evidence_source_must_match_issue_category() {
        assert!(evidence_allowed_for_category(
            "gb-t-15834-2011",
            IssueCategory::Punctuation
        ));
        assert!(!evidence_allowed_for_category(
            "gb-t-15834-2011",
            IssueCategory::Grammar
        ));
    }

    #[test]
    fn independent_reclassification_is_reviewable_but_not_confirmed() {
        let document = ExtractedDocument {
            format: textcomb_domain::DocumentFormat::Txt,
            text: "他心情很繁重。".to_owned(),
            segments: vec![crate::documents::SourceSegment {
                char_start: 0,
                char_end: 7,
                page: None,
                line: Some(1),
                paragraph_index: 0,
            }],
            char_count: 7,
        };
        let candidate = ResolvedCandidate {
            candidate_index: 0,
            source_start: 1,
            source_end: 6,
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            quote: "心情很繁重".to_owned(),
            reason: "搭配".to_owned(),
            suggestion: "心情很沉重".to_owned(),
            candidate_confidence: 95,
            protected: false,
        };
        let verdict = VerifiedCandidate {
            candidate_index: 0,
            category: Some(IssueCategory::Grammar),
            grammar_subtype: Some(GrammarSubtype::WordMisuse),
            verdict: VerificationVerdict::Confirmed,
            confidence: 95,
            reason: "复核后认为词语使用不当".to_owned(),
            suggestion: "心情很沉重".to_owned(),
            evidence_source_ids: Vec::new(),
        };
        let issues = finalize_issues(
            &document,
            vec![candidate],
            vec![verdict],
            &HashMap::new(),
            80,
            50,
        )
        .unwrap();
        assert_eq!(issues[0].grammar_subtype, Some(GrammarSubtype::WordMisuse));
        assert_eq!(issues[0].level, IssueLevel::Suspected);
    }
}
