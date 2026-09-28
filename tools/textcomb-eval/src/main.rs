use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use textcomb_domain::{GrammarSubtype, IssueCategory, IssueLevel, ReportV1};

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

    for document in &gold {
        let report_path = cli
            .predictions
            .join(format!("{}.json", document.document_id));
        let predictions = if report_path.exists() {
            load_predictions(&report_path)?
        } else {
            Vec::new()
        };

        accumulate(
            &document.issues,
            predictions
                .iter()
                .filter(|issue| issue.level == IssueLevel::Confirmed),
            &mut confirmed,
        );
        accumulate(
            &document.issues,
            predictions
                .iter()
                .filter(|issue| issue.level == IssueLevel::Suspected),
            &mut suspected_only,
        );
        accumulate(&document.issues, predictions.iter(), &mut combined);

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
        schema: "textcomb.evaluation.v2",
        documents: gold.len(),
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

fn load_predictions(path: &Path) -> Result<Vec<Prediction>> {
    let report: ReportV1 = serde_json::from_reader(
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
    )
    .with_context(|| format!("invalid ReportV1 at {}", path.display()))?;
    if !report.complete {
        bail!("incomplete report cannot be evaluated: {}", path.display());
    }
    Ok(report
        .issues
        .into_iter()
        .map(|issue| Prediction {
            category: issue.category,
            grammar_subtype: issue.grammar_subtype,
            level: issue.level,
            char_start: issue.location.char_start,
            char_end: issue.location.char_end,
        })
        .collect())
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
    let mut candidates = Vec::new();
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
                candidates.push((overlap, prediction_index, gold_index));
            }
        }
    }
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    let mut matched_predictions = HashSet::new();
    let mut matched_gold = HashSet::new();
    for (_, prediction_index, gold_index) in candidates {
        if matched_predictions.contains(&prediction_index) || matched_gold.contains(&gold_index) {
            continue;
        }
        matched_predictions.insert(prediction_index);
        matched_gold.insert(gold_index);
        counts.true_positive += 1;
    }
    counts.false_positive += (predictions.len() - matched_predictions.len()) as u64;
    counts.false_negative += (gold.len() - matched_gold.len()) as u64;
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
