use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use textcomb_domain::{
    GrammarSubtype, IssueCategory, IssueLevel, REPORT_SCHEMA_V1, ReportSummary, ReportV1,
};

#[derive(Debug, Parser)]
#[command(
    name = "textcomb-eval",
    about = "Evaluate TextComb reports against private adjudicated labels"
)]
struct Cli {
    /// JSONL file containing one adjudicated document per line.
    #[arg(long)]
    gold: PathBuf,
    /// Directory containing ReportV1 JSON files named <document_id>.json.
    #[arg(long)]
    predictions: PathBuf,
    /// Optional path for the machine-readable evaluation summary.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct GoldDocument {
    document_id: String,
    issues: Vec<GoldIssue>,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldIssue {
    category: IssueCategory,
    grammar_subtype: Option<GrammarSubtype>,
    char_start: u32,
    char_end: u32,
}

#[derive(Debug, Clone)]
struct Prediction {
    category: IssueCategory,
    grammar_subtype: Option<GrammarSubtype>,
    level: IssueLevel,
    char_start: u32,
    char_end: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Counts {
    true_positive: u64,
    false_positive: u64,
    false_negative: u64,
}

#[derive(Debug, Clone, Serialize)]
struct Metrics {
    precision: f64,
    recall: f64,
    f1: f64,
    counts: Counts,
}

#[derive(Debug, Serialize)]
struct EvaluationSummary {
    schema: &'static str,
    documents: usize,
    language_categories: [&'static str; 3],
    confirmed: Metrics,
    suspected_only: Metrics,
    combined: Metrics,
    combined_by_category: BTreeMap<String, Metrics>,
    grammar_detection: Metrics,
    combined_by_grammar_subtype: BTreeMap<String, Metrics>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let gold = load_gold(&cli.gold)?;
    let mut confirmed = Counts::default();
    let mut suspected_only = Counts::default();
    let mut combined = Counts::default();
    let mut by_category: HashMap<IssueCategory, Counts> = HashMap::new();
    let mut grammar_detection = Counts::default();
    let mut by_grammar_subtype: HashMap<GrammarSubtype, Counts> = HashMap::new();
    let mut report_ids = HashSet::new();
    let mut job_ids = HashSet::new();

    for document in &gold {
        let report_path = cli
            .predictions
            .join(format!("{}.json", document.document_id));
        // Missing reports are failed coverage, not evidence of a clean article.
        let report = load_report(&report_path, document)?;
        if !report_ids.insert(report.report_id) || !job_ids.insert(report.job_id) {
            bail!("report or job reused for {}", document.document_id);
        }
        let predictions: Vec<_> = report
            .issues
            .into_iter()
            .map(|issue| Prediction {
                category: issue.category,
                grammar_subtype: issue.grammar_subtype,
                level: issue.level,
                char_start: issue.location.char_start,
                char_end: issue.location.char_end,
            })
            .collect();
        let language_gold: Vec<_> = document
            .issues
            .iter()
            .filter(|issue| issue.category != IssueCategory::Paragraph)
            .cloned()
            .collect();

        accumulate(
            &language_gold,
            predictions.iter().filter(|issue| {
                issue.level == IssueLevel::Confirmed && issue.category != IssueCategory::Paragraph
            }),
            &mut confirmed,
        );
        accumulate(
            &language_gold,
            predictions.iter().filter(|issue| {
                issue.level == IssueLevel::Suspected && issue.category != IssueCategory::Paragraph
            }),
            &mut suspected_only,
        );
        accumulate(
            &language_gold,
            predictions
                .iter()
                .filter(|issue| issue.category != IssueCategory::Paragraph),
            &mut combined,
        );

        let grammar_gold: Vec<_> = document
            .issues
            .iter()
            .filter(|issue| issue.category == IssueCategory::Grammar)
            .cloned()
            .collect();
        accumulate_with_subtype_policy(
            &grammar_gold,
            predictions
                .iter()
                .filter(|issue| issue.category == IssueCategory::Grammar),
            &mut grammar_detection,
            false,
        );
        for subtype in grammar_subtypes() {
            let subtype_gold: Vec<_> = grammar_gold
                .iter()
                .filter(|issue| issue.grammar_subtype == Some(subtype))
                .cloned()
                .collect();
            accumulate(
                &subtype_gold,
                predictions
                    .iter()
                    .filter(|issue| issue.grammar_subtype == Some(subtype)),
                by_grammar_subtype.entry(subtype).or_default(),
            );
        }

        for category in [
            IssueCategory::Typo,
            IssueCategory::Punctuation,
            IssueCategory::Grammar,
            IssueCategory::Paragraph,
        ] {
            let category_gold: Vec<_> = document
                .issues
                .iter()
                .filter(|issue| issue.category == category)
                .cloned()
                .collect();
            let category_predictions = predictions
                .iter()
                .filter(|issue| issue.category == category);
            accumulate(
                &category_gold,
                category_predictions,
                by_category.entry(category).or_default(),
            );
        }
    }

    let summary = EvaluationSummary {
        schema: "textcomb.evaluation.v3",
        documents: gold.len(),
        language_categories: ["typo", "punctuation", "grammar"],
        confirmed: metrics(confirmed),
        suspected_only: metrics(suspected_only),
        combined: metrics(combined),
        combined_by_category: by_category
            .into_iter()
            .map(|(category, counts)| (category.as_str().to_owned(), metrics(counts)))
            .collect(),
        grammar_detection: metrics(grammar_detection),
        combined_by_grammar_subtype: by_grammar_subtype
            .into_iter()
            .map(|(subtype, counts)| (subtype.as_str().to_owned(), metrics(counts)))
            .collect(),
    };
    let json = serde_json::to_string_pretty(&summary)?;
    if let Some(output) = cli.output {
        std::fs::write(&output, &json)
            .with_context(|| format!("failed to write {}", output.display()))?;
    }
    println!("{json}");
    Ok(())
}

fn load_gold(path: &Path) -> Result<Vec<GoldDocument>> {
    let reader = BufReader::new(
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
    );
    let mut documents = Vec::new();
    let mut ids = HashSet::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let document: GoldDocument = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON on line {}", index + 1))?;
        validate_document_id(&document.document_id)?;
        if !ids.insert(document.document_id.clone()) {
            bail!("duplicate document_id {}", document.document_id);
        }
        validate_ranges(
            document
                .issues
                .iter()
                .map(|issue| (issue.char_start, issue.char_end)),
            &document.document_id,
        )?;
        for issue in &document.issues {
            if (issue.category == IssueCategory::Grammar) != issue.grammar_subtype.is_some() {
                bail!(
                    "grammar_subtype is required only for grammar issues in {}",
                    document.document_id
                );
            }
        }
        documents.push(document);
    }
    if documents.is_empty() {
        bail!("gold JSONL contains no documents");
    }
    Ok(documents)
}

fn validate_document_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("document_id must contain 1..128 ASCII letters, digits, hyphens or underscores");
    }
    Ok(())
}

fn load_report(path: &Path, gold: &GoldDocument) -> Result<ReportV1> {
    let report: ReportV1 = serde_json::from_reader(
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
    )
    .with_context(|| format!("invalid ReportV1 at {}", path.display()))?;
    validate_report(&report, gold)
        .with_context(|| format!("invalid report at {}", path.display()))?;
    Ok(report)
}

fn validate_report(report: &ReportV1, gold: &GoldDocument) -> Result<()> {
    if report.schema != REPORT_SCHEMA_V1 || !report.complete {
        bail!("only complete ReportV1 reports can be evaluated");
    }
    if report.summary != ReportSummary::from_issues(&report.issues) {
        bail!("report summary disagrees with issues");
    }
    validate_ranges(
        report
            .issues
            .iter()
            .map(|issue| (issue.location.char_start, issue.location.char_end)),
        &gold.document_id,
    )?;
    let mut issue_ids = HashSet::new();
    for issue in &report.issues {
        if !issue_ids.insert(issue.id) || issue.location.char_end > report.document.char_count {
            bail!(
                "duplicate issue ID or out-of-bounds prediction in {}",
                gold.document_id
            );
        }
        if issue.original_text != issue.location.quote
            || issue.original_text.chars().count() as u64
                != u64::from(issue.location.char_end - issue.location.char_start)
        {
            bail!(
                "prediction quote disagrees with its range in {}",
                gold.document_id
            );
        }
        if (issue.category == IssueCategory::Grammar) != issue.grammar_subtype.is_some() {
            bail!("prediction grammar_subtype is required only for grammar issues");
        }
    }
    if gold
        .issues
        .iter()
        .any(|issue| issue.char_end > report.document.char_count)
    {
        bail!("gold range exceeds source length in {}", gold.document_id);
    }
    Ok(())
}

fn validate_ranges(ranges: impl Iterator<Item = (u32, u32)>, document_id: &str) -> Result<()> {
    for (start, end) in ranges {
        if start >= end {
            bail!("invalid issue range {start}..{end} in {document_id}");
        }
    }
    Ok(())
}

fn accumulate<'a>(
    gold: &[GoldIssue],
    predictions: impl Iterator<Item = &'a Prediction>,
    counts: &mut Counts,
) {
    accumulate_with_subtype_policy(gold, predictions, counts, true);
}

fn accumulate_with_subtype_policy<'a>(
    gold: &[GoldIssue],
    predictions: impl Iterator<Item = &'a Prediction>,
    counts: &mut Counts,
    require_subtype: bool,
) {
    let predictions: Vec<_> = predictions.collect();
    let mut candidates = vec![Vec::new(); predictions.len()];
    for (prediction_index, prediction) in predictions.iter().enumerate() {
        for (gold_index, expected) in gold.iter().enumerate() {
            if prediction.category != expected.category
                || (require_subtype
                    && prediction.category == IssueCategory::Grammar
                    && prediction.grammar_subtype != expected.grammar_subtype)
            {
                continue;
            }
            let overlap_start = prediction.char_start.max(expected.char_start);
            let overlap_end = prediction.char_end.min(expected.char_end);
            let overlap = overlap_end.saturating_sub(overlap_start);
            let prediction_len = prediction.char_end.saturating_sub(prediction.char_start);
            let gold_len = expected.char_end.saturating_sub(expected.char_start);
            if overlap > 0
                && u64::from(overlap) * 2 >= u64::from(prediction_len)
                && u64::from(overlap) * 2 >= u64::from(gold_len)
            {
                candidates[prediction_index].push((overlap, gold_index));
            }
        }
    }
    for edges in &mut candidates {
        edges.sort_unstable_by(|left, right| right.cmp(left));
    }
    // Reassign earlier matches when needed: greedy overlap order can undercount.
    let mut owners = vec![None; gold.len()];
    let mut matched = 0;
    for prediction_index in 0..predictions.len() {
        if assign(
            prediction_index,
            &candidates,
            &mut owners,
            &mut vec![false; gold.len()],
        ) {
            matched += 1;
        }
    }
    counts.true_positive += matched as u64;
    counts.false_positive += (predictions.len() - matched) as u64;
    counts.false_negative += (gold.len() - matched) as u64;
}

fn assign(
    prediction: usize,
    candidates: &[Vec<(u32, usize)>],
    owners: &mut [Option<usize>],
    visited: &mut [bool],
) -> bool {
    for &(_, gold) in &candidates[prediction] {
        if visited[gold] {
            continue;
        }
        visited[gold] = true;
        if owners[gold].is_none_or(|previous| assign(previous, candidates, owners, visited)) {
            owners[gold] = Some(prediction);
            return true;
        }
    }
    false
}

fn grammar_subtypes() -> [GrammarSubtype; 10] {
    [
        GrammarSubtype::WordOrder,
        GrammarSubtype::Collocation,
        GrammarSubtype::MissingComponent,
        GrammarSubtype::RedundantComponent,
        GrammarSubtype::MissingOrRedundantComponent,
        GrammarSubtype::MixedStructure,
        GrammarSubtype::Ambiguity,
        GrammarSubtype::Illogical,
        GrammarSubtype::Conjunction,
        GrammarSubtype::WordMisuse,
    ]
}

fn metrics(counts: Counts) -> Metrics {
    let precision = ratio(
        counts.true_positive,
        counts.true_positive + counts.false_positive,
    );
    let recall = ratio(
        counts.true_positive,
        counts.true_positive + counts.false_negative,
    );
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Metrics {
        precision,
        recall,
        f1,
        counts,
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_cannot_escape_prediction_directory() {
        for id in ["", "../article", "a/b", "a\\b", "C:article", ".", ".."] {
            assert!(validate_document_id(id).is_err());
        }
        assert!(validate_document_id("article-001_A").is_ok());
    }

    #[test]
    fn missing_report_is_not_a_clean_prediction() {
        let gold = GoldDocument {
            document_id: "missing".into(),
            issues: vec![],
        };
        let path = std::env::temp_dir().join(format!(
            "textcomb-missing-report-{}.json",
            std::process::id()
        ));
        assert!(load_report(&path, &gold).is_err());
    }

    #[test]
    fn report_contract_and_gold_bounds_are_checked() {
        let mut report: ReportV1 = serde_json::from_value(serde_json::json!({
            "schema": REPORT_SCHEMA_V1,
            "report_id": "00000000-0000-4000-8000-000000000001",
            "job_id": "00000000-0000-4000-8000-000000000002",
            "document": {"original_name":"synthetic.txt","format":"txt","char_count":2},
            "analysis": {"provider_kind":"synthetic","candidate_model":"synthetic","verifier_model":"synthetic","prompt_version":"test","analyzer_version":"test","reference_profile":false},
            "summary": {"total":0,"confirmed":0,"suspected":0,"typo":0,"punctuation":0,"grammar":0,"paragraph":0},
            "issues": [], "complete":true, "generated_at":"2026-01-01T00:00:00Z"
        })).expect("synthetic ReportV1");
        let mut gold = GoldDocument {
            document_id: "synthetic".into(),
            issues: vec![],
        };
        assert!(validate_report(&report, &gold).is_ok());
        report.complete = false;
        assert!(validate_report(&report, &gold).is_err());
        report.complete = true;
        report.schema = "unknown".into();
        assert!(validate_report(&report, &gold).is_err());
        report.schema = REPORT_SCHEMA_V1.into();
        report.summary.total = 1;
        assert!(validate_report(&report, &gold).is_err());
        report.summary.total = 0;
        gold.issues.push(GoldIssue {
            category: IssueCategory::Typo,
            grammar_subtype: None,
            char_start: 1,
            char_end: 3,
        });
        assert!(validate_report(&report, &gold).is_err());
    }

    #[test]
    fn matching_reassigns_an_earlier_overlap_instead_of_losing_a_true_positive() {
        let gold = [
            GoldIssue {
                category: IssueCategory::Typo,
                grammar_subtype: None,
                char_start: 0,
                char_end: 12,
            },
            GoldIssue {
                category: IssueCategory::Typo,
                grammar_subtype: None,
                char_start: 10,
                char_end: 22,
            },
        ];
        let predictions = [
            Prediction {
                category: IssueCategory::Typo,
                grammar_subtype: None,
                level: IssueLevel::Confirmed,
                char_start: 0,
                char_end: 20,
            },
            Prediction {
                category: IssueCategory::Typo,
                grammar_subtype: None,
                level: IssueLevel::Confirmed,
                char_start: 0,
                char_end: 6,
            },
        ];
        for predictions in [
            predictions.to_vec(),
            predictions.into_iter().rev().collect(),
        ] {
            let mut counts = Counts::default();
            accumulate(&gold, predictions.iter(), &mut counts);
            assert_eq!(
                (
                    counts.true_positive,
                    counts.false_positive,
                    counts.false_negative
                ),
                (2, 0, 0)
            );
        }
    }

    #[test]
    fn overlapping_predictions_are_matched_one_to_one() {
        let gold = vec![GoldIssue {
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            char_start: 5,
            char_end: 10,
        }];
        let predictions = [
            Prediction {
                category: IssueCategory::Grammar,
                grammar_subtype: Some(GrammarSubtype::Collocation),
                level: IssueLevel::Confirmed,
                char_start: 5,
                char_end: 8,
            },
            Prediction {
                category: IssueCategory::Grammar,
                grammar_subtype: Some(GrammarSubtype::Collocation),
                level: IssueLevel::Confirmed,
                char_start: 7,
                char_end: 10,
            },
        ];
        let mut counts = Counts::default();
        accumulate(&gold, predictions.iter(), &mut counts);
        assert_eq!(counts.true_positive, 1);
        assert_eq!(counts.false_positive, 1);
        assert_eq!(counts.false_negative, 0);
    }

    #[test]
    fn wrong_grammar_subtype_is_not_a_match() {
        let gold = [GoldIssue {
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            char_start: 5,
            char_end: 10,
        }];
        let predictions = [Prediction {
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::WordOrder),
            level: IssueLevel::Confirmed,
            char_start: 5,
            char_end: 10,
        }];
        let mut strict = Counts::default();
        accumulate(&gold, predictions.iter(), &mut strict);
        assert_eq!(
            (
                strict.true_positive,
                strict.false_positive,
                strict.false_negative
            ),
            (0, 1, 1)
        );
        let mut detection = Counts::default();
        accumulate_with_subtype_policy(&gold, predictions.iter(), &mut detection, false);
        assert_eq!(detection.true_positive, 1);
    }

    #[test]
    fn tiny_overlap_or_overbroad_span_is_not_a_match() {
        let gold = [GoldIssue {
            category: IssueCategory::Grammar,
            grammar_subtype: Some(GrammarSubtype::Collocation),
            char_start: 5,
            char_end: 15,
        }];
        let predictions = [
            Prediction {
                category: IssueCategory::Grammar,
                grammar_subtype: Some(GrammarSubtype::Collocation),
                level: IssueLevel::Confirmed,
                char_start: 14,
                char_end: 20,
            },
            Prediction {
                category: IssueCategory::Grammar,
                grammar_subtype: Some(GrammarSubtype::Collocation),
                level: IssueLevel::Confirmed,
                char_start: 0,
                char_end: 40,
            },
        ];
        let mut counts = Counts::default();
        accumulate(&gold, predictions.iter(), &mut counts);
        assert_eq!(
            (
                counts.true_positive,
                counts.false_positive,
                counts.false_negative
            ),
            (0, 2, 1)
        );
    }
}
