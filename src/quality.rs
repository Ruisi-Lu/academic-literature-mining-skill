use chrono::{Datelike, Utc};

use crate::domain::{QualityAssessment, ResearchPlan, WorkRecord};
use crate::util::{normalize_arxiv, normalize_doi, normalize_openalex, now};

pub fn assess(record: &WorkRecord, plan: &ResearchPlan) -> QualityAssessment {
    let mut score: f64 = 0.0;
    let mut signals = Vec::new();
    let mut rejection_reasons = Vec::new();
    let current_year = Utc::now().year().max(0) as u32;

    if record.flags.get("is_retracted").copied().unwrap_or(false) {
        rejection_reasons.push("retracted or withdrawn record".to_owned());
    }
    if record.flags.get("is_paratext").copied().unwrap_or(false) {
        rejection_reasons.push("paratext rather than a scholarly work".to_owned());
    }
    if record.title.trim().is_empty() {
        rejection_reasons.push("missing title".to_owned());
    } else {
        score += 5.0;
    }
    if !record.authors.iter().any(|author| {
        !author.literal.trim().is_empty()
            || !author.family.trim().is_empty()
            || !author.given.trim().is_empty()
    }) {
        rejection_reasons.push("missing identifiable authorship".to_owned());
    } else {
        score += 7.0;
    }
    if record.year().is_none() {
        rejection_reasons.push("missing publication year".to_owned());
    } else {
        score += 5.0;
    }
    if record.abstract_text.trim().is_empty() {
        rejection_reasons.push("missing abstract for relevance screening".to_owned());
    } else {
        score += 8.0;
    }

    if let Some(year) = record.year() {
        let before_start = plan
            .date_from
            .as_deref()
            .and_then(|value| value.get(..4))
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|start| year < start);
        let after_end = plan
            .date_to
            .as_deref()
            .and_then(|value| value.get(..4))
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|end| year > end);
        if before_start || after_end {
            rejection_reasons.push("publication year outside research plan".to_owned());
        }
        if year > current_year.saturating_add(1) {
            rejection_reasons.push("publication year is implausibly far in the future".to_owned());
        }
    }
    if !plan.languages.is_empty()
        && !record.language.trim().is_empty()
        && !plan.languages.iter().any(|language| {
            let record_primary = record
                .language
                .split('-')
                .next()
                .unwrap_or(&record.language);
            let plan_primary = language.split('-').next().unwrap_or(language);
            record.language.eq_ignore_ascii_case(language)
                || record_primary.eq_ignore_ascii_case(plan_primary)
        })
    {
        rejection_reasons.push("language excluded by research plan".to_owned());
    }

    let scholarly_weight = match record.work_type.as_str() {
        "article-journal" => 20.0,
        "paper-conference" => 17.0,
        "chapter" | "book" | "thesis" | "report" => 12.0,
        "preprint" if plan.include_preprints => 4.0,
        "preprint" => {
            rejection_reasons.push("preprints excluded by research plan".to_owned());
            0.0
        }
        "dataset" | "editorial" | "peer-review" | "paratext" | "journal" | "proceedings"
        | "reference-entry" | "grant" | "standard" | "component" | "other" => {
            rejection_reasons.push(format!("unsupported scholarly type: {}", record.work_type));
            0.0
        }
        _ => 7.0,
    };
    score += scholarly_weight;
    signals.push(format!(
        "scholarly-type:{}:+{scholarly_weight:.0}",
        record.work_type
    ));

    if record
        .ids
        .get("doi")
        .and_then(|value| normalize_doi(value))
        .is_some()
    {
        score += 8.0;
        signals.push("persistent-identifier:doi:+8".to_owned());
    } else if record
        .ids
        .get("arxiv")
        .and_then(|value| normalize_arxiv(value))
        .is_some()
        || record
            .ids
            .get("openalex")
            .and_then(|value| normalize_openalex(value))
            .is_some()
    {
        score += 4.0;
        signals.push("persistent-identifier:repository:+4".to_owned());
    } else {
        rejection_reasons.push("missing valid persistent identifier".to_owned());
    }

    if !record.container_title.is_empty() {
        score += 5.0;
        signals.push("venue-metadata:+5".to_owned());
    }

    let age = record
        .year()
        .map(|year| current_year.saturating_sub(year).max(1))
        .unwrap_or(1);
    let citations_per_year = record.metrics.cited_by_count as f64 / age as f64;
    let citation_score = (citations_per_year + 1.0).ln() / 5.0_f64.ln() * 18.0;
    let citation_score = citation_score.clamp(0.0, 18.0);
    score += citation_score;
    signals.push(format!(
        "age-normalized-citations:{citations_per_year:.2}/year:+{citation_score:.1}"
    ));

    if let Some(percentile) = record.metrics.citation_percentile {
        let normalized = if percentile > 1.0 {
            percentile / 100.0
        } else {
            percentile
        };
        let points = normalized.clamp(0.0, 1.0) * 12.0;
        score += points;
        signals.push(format!("citation-percentile:+{points:.1}"));
    }
    if let Some(fwci) = record.metrics.fwci {
        let points = (fwci.max(0.0) + 1.0).ln().min(2.5) / 2.5 * 8.0;
        score += points;
        signals.push(format!("field-weighted-citation-impact:+{points:.1}"));
    }
    if record.metrics.influential_citation_count > 0 {
        let points = ((record.metrics.influential_citation_count as f64 + 1.0).ln() * 2.0).min(7.0);
        score += points;
        signals.push(format!("influential-citations:+{points:.1}"));
    }

    let title = record.title.to_lowercase();
    let review_markers = [
        "systematic review",
        "meta-analysis",
        "meta analysis",
        "scoping review",
        "survey",
    ];
    if review_markers.iter().any(|marker| title.contains(marker)) {
        score += 7.0;
        signals.push("evidence-synthesis:+7".to_owned());
    }

    let authorized_pdf = record
        .fulltext_candidates
        .iter()
        .any(|candidate| candidate.authorized);
    if authorized_pdf {
        score += 5.0;
        signals.push("authorized-fulltext:+5".to_owned());
    } else {
        rejection_reasons.push("no authorized open-access PDF candidate".to_owned());
    }

    score = score.clamp(0.0, 100.0);
    let tier = match score {
        value if value >= 80.0 => "A",
        value if value >= 65.0 => "B",
        value if value >= 50.0 => "C",
        _ => "D",
    }
    .to_owned();
    if score < plan.min_quality_score {
        rejection_reasons.push("below configured academic-value threshold".to_owned());
    }
    QualityAssessment {
        score,
        tier,
        relevance_score: 0.0,
        relevance_logit: None,
        priority_score: 0.0,
        accepted: rejection_reasons.is_empty(),
        signals,
        rejection_reasons,
        screened_at: now(),
    }
}

pub fn add_relevance(
    mut assessment: QualityAssessment,
    relevance_logit: f64,
    relevance_score: f64,
    plan: &ResearchPlan,
) -> QualityAssessment {
    assessment.relevance_logit = Some(relevance_logit);
    assessment.relevance_score = relevance_score.clamp(0.0, 1.0);
    assessment.priority_score =
        assessment.relevance_score * 0.55 + (assessment.score / 100.0) * 0.45;
    if assessment.relevance_score < plan.min_relevance_score {
        assessment
            .rejection_reasons
            .push("below configured relevance threshold".to_owned());
    }
    assessment.accepted = assessment.rejection_reasons.is_empty()
        && assessment.score >= plan.min_quality_score
        && assessment.relevance_score >= plan.min_relevance_score;
    assessment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Author, FullTextCandidate, ResearchPlan, WorkRecord};

    fn plan() -> ResearchPlan {
        ResearchPlan {
            research_question: "question".into(),
            queries: vec!["query".into()],
            inclusion_criteria: vec![],
            exclusion_criteria: vec![],
            date_from: None,
            date_to: None,
            languages: vec![],
            sources: vec!["crossref".into()],
            include_preprints: false,
            target_papers: 10,
            min_quality_score: 40.0,
            min_relevance_score: 0.2,
        }
    }

    #[test]
    fn retracted_work_never_passes() {
        let mut record = WorkRecord::new("test", "one");
        record.title = "A credible study".into();
        record.abstract_text = "Abstract".into();
        record.authors.push(Author::default());
        record.issued.date_parts = vec![vec![2020]];
        record.flags.insert("is_retracted".into(), true);
        record.fulltext_candidates.push(FullTextCandidate {
            authorized: true,
            ..FullTextCandidate::default()
        });
        assert!(!assess(&record, &plan()).accepted);
    }

    #[test]
    fn relevance_keeps_the_raw_logit_and_normalized_score() {
        let assessment = QualityAssessment {
            score: 80.0,
            accepted: true,
            ..QualityAssessment::default()
        };
        let ranked = add_relevance(assessment, -3.5, 0.029312, &plan());
        assert_eq!(ranked.relevance_logit, Some(-3.5));
        assert_eq!(ranked.relevance_score, 0.029312);
        assert!(!ranked.accepted);
    }
}
