use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use super::types::{Dataset, Document, Query};

#[derive(Debug, Deserialize)]
struct LongMemEvalEntry {
    question_id: String,
    question_type: String,
    question: String,
    question_date: String,
    haystack_session_ids: Vec<String>,
    haystack_dates: Vec<String>,
    haystack_sessions: Vec<Vec<LongMemEvalTurn>>,
    answer_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LongMemEvalTurn {
    role: String,
    content: String,
}

pub(super) fn try_convert(
    content: &str,
    include_abstention: bool,
    max_sessions_per_query: Option<usize>,
    max_session_chars: Option<usize>,
) -> Result<Option<Dataset>, String> {
    let entries: Vec<LongMemEvalEntry> = match serde_json::from_str(content) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };
    if entries.is_empty() {
        return Err("LongMemEval dataset has no entries".to_string());
    }
    convert_entries(
        entries,
        include_abstention,
        max_sessions_per_query,
        max_session_chars,
    )
    .map(Some)
}

fn convert_entries(
    entries: Vec<LongMemEvalEntry>,
    include_abstention: bool,
    max_sessions_per_query: Option<usize>,
    max_session_chars: Option<usize>,
) -> Result<Dataset, String> {
    let mut queries = Vec::with_capacity(entries.len());
    let mut docs = HashMap::new();
    let mut skipped_abs = 0usize;
    let mut skipped_no_relevant = 0usize;
    let mut conflicting_sessions = 0usize;
    for entry in entries {
        if !include_abstention && entry.question_id.ends_with("_abs") {
            skipped_abs += 1;
            continue;
        }

        validate_entry_lengths(&entry)?;
        let selected_indexes = select_indexes(&entry, max_sessions_per_query);
        let (selected_ids, conflicts) =
            upsert_documents(&entry, &selected_indexes, max_session_chars, &mut docs);
        conflicting_sessions += conflicts;
        let relevant = collect_relevant_ids(&entry, &selected_ids);
        if relevant.is_empty() {
            skipped_no_relevant += 1;
            continue;
        }

        queries.push(Query {
            id: entry.question_id.clone(),
            query: build_query_text(&entry),
            relevant,
            relevance_grades: std::collections::HashMap::new(),
        });
    }

    if queries.is_empty() {
        return Err("LongMemEval conversion produced zero queries".to_string());
    }

    let mut documents: Vec<Document> = docs
        .into_iter()
        .map(|(id, text)| Document {
            id,
            text,
            doc_type: "doc".to_string(),
        })
        .collect();
    documents.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Dataset {
        description: "LongMemEval session-retrieval benchmark converted to memd benchmark format"
            .to_string(),
        version: "longmemeval-converted-v1".to_string(),
        note: Some(format!(
            "abstention_included={}, max_sessions_per_query={:?}, max_session_chars={:?}, skipped_abs={}, skipped_no_relevant={}, conflicting_sessions={}",
            include_abstention,
            max_sessions_per_query,
            max_session_chars,
            skipped_abs,
            skipped_no_relevant,
            conflicting_sessions
        )),
        queries,
        documents,
    })
}

fn validate_entry_lengths(entry: &LongMemEvalEntry) -> Result<(), String> {
    let n_ids = entry.haystack_session_ids.len();
    if entry.haystack_dates.len() != n_ids || entry.haystack_sessions.len() != n_ids {
        return Err(format!(
            "entry {} has inconsistent haystack lengths (ids={}, dates={}, sessions={})",
            entry.question_id,
            n_ids,
            entry.haystack_dates.len(),
            entry.haystack_sessions.len()
        ));
    }
    Ok(())
}

fn select_indexes(entry: &LongMemEvalEntry, max_sessions_per_query: Option<usize>) -> Vec<usize> {
    let n = entry.haystack_session_ids.len();
    let Some(cap) = max_sessions_per_query else {
        return (0..n).collect();
    };
    if n <= cap {
        return (0..n).collect();
    }

    let by_id = session_index_map(entry);
    let mut selected = HashSet::new();
    for answer_id in &entry.answer_session_ids {
        if let Some(idx) = by_id.get(answer_id.as_str()) {
            selected.insert(*idx);
            if selected.len() >= cap {
                break;
            }
        }
    }

    let mut idx = n;
    while selected.len() < cap && idx > 0 {
        idx -= 1;
        selected.insert(idx);
    }

    let mut indexes: Vec<usize> = selected.into_iter().collect();
    indexes.sort_unstable();
    indexes
}

fn session_index_map(entry: &LongMemEvalEntry) -> HashMap<&str, usize> {
    let mut by_id = HashMap::with_capacity(entry.haystack_session_ids.len());
    for (idx, session_id) in entry.haystack_session_ids.iter().enumerate() {
        by_id.insert(session_id.as_str(), idx);
    }
    by_id
}

fn upsert_documents(
    entry: &LongMemEvalEntry,
    indexes: &[usize],
    max_session_chars: Option<usize>,
    docs: &mut HashMap<String, String>,
) -> (HashSet<String>, usize) {
    let mut selected_ids = HashSet::with_capacity(indexes.len());
    let mut conflicts = 0usize;
    for &idx in indexes {
        let session_id = &entry.haystack_session_ids[idx];
        let doc_id = format!("session:{session_id}");
        let mut doc_text =
            render_session(&entry.haystack_dates[idx], &entry.haystack_sessions[idx]);
        if let Some(limit) = max_session_chars {
            doc_text = truncate_chars(&doc_text, limit);
        }
        if let Some(existing) = docs.get(&doc_id) {
            if existing != &doc_text {
                conflicts += 1;
                if doc_text.len() > existing.len() {
                    docs.insert(doc_id.clone(), doc_text);
                }
            }
        } else {
            docs.insert(doc_id.clone(), doc_text);
        }
        selected_ids.insert(doc_id);
    }
    (selected_ids, conflicts)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn collect_relevant_ids(entry: &LongMemEvalEntry, selected_ids: &HashSet<String>) -> Vec<String> {
    let mut relevant = Vec::with_capacity(entry.answer_session_ids.len());
    for answer_id in &entry.answer_session_ids {
        let doc_id = format!("session:{answer_id}");
        if selected_ids.contains(&doc_id) {
            relevant.push(doc_id);
        }
    }
    relevant.sort_unstable();
    relevant.dedup();
    relevant
}

fn build_query_text(entry: &LongMemEvalEntry) -> String {
    format!(
        "{}\n[question_type: {}]\n[question_date: {}]",
        entry.question.trim(),
        entry.question_type,
        entry.question_date
    )
}

fn render_session(session_date: &str, turns: &[LongMemEvalTurn]) -> String {
    let mut text = String::new();
    text.push_str("session_date: ");
    text.push_str(session_date.trim());
    text.push('\n');
    for turn in turns {
        text.push_str(turn.role.trim());
        text.push_str(": ");
        text.push_str(turn.content.trim());
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_longmemeval_payload() {
        let payload = serde_json::json!([
            {
                "question_id": "q1",
                "question_type": "single-session-user",
                "question": "What degree did I graduate with?",
                "answer": "Business Administration",
                "question_date": "2023/05/01",
                "haystack_session_ids": ["s1", "s2"],
                "haystack_dates": ["2023/04/20", "2023/04/25"],
                "haystack_sessions": [
                    [{"role":"user","content":"I graduated with Business Administration."}],
                    [{"role":"user","content":"I also like hiking."}]
                ],
                "answer_session_ids": ["s1"]
            }
        ]);

        let converted = try_convert(&payload.to_string(), false, None, None)
            .expect("conversion should succeed")
            .expect("payload should be recognized as LongMemEval");
        assert_eq!(converted.queries.len(), 1);
        assert_eq!(converted.documents.len(), 2);
        assert_eq!(
            converted.queries[0].relevant,
            vec!["session:s1".to_string()]
        );
        assert!(converted.documents[0].id.starts_with("session:"));
    }

    #[test]
    fn skips_abstention_by_default() {
        let payload = serde_json::json!([
            {
                "question_id": "q_abs",
                "question_type": "single-session-user",
                "question": "What did I say?",
                "answer": "N/A",
                "question_date": "2023/05/01",
                "haystack_session_ids": ["s1"],
                "haystack_dates": ["2023/04/20"],
                "haystack_sessions": [[{"role":"user","content":"hello"}]],
                "answer_session_ids": ["s1"]
            }
        ]);

        let err = try_convert(&payload.to_string(), false, None, None)
            .expect_err("all-abstention payload should fail after filtering");
        assert!(err.contains("zero queries"));
    }
}
