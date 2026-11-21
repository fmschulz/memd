//! One-call capture of a solved problem from a case file.
//!
//! The case file is the exchange format shared with other tools (see
//! `docs/experience-memory.md`). [`record_case`] turns it into the same
//! typed records the four-step audit path writes: a problem, one attempt per
//! failed approach plus one for the fix, a check on the fix, and a lesson.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::recall::EXHAUSTIVE_LIMIT;
use super::{
    ensure_project, event_time, get_experience, lesson_from_event, record_observed_check_unlocked,
    record_unlocked, required_event, resolve_case, sort_artifacts, validation, ConditionalLesson,
    ExperienceContext, ExperienceEvent, LessonApplicability, LessonEvidence, LessonVisibility,
    ObservedCheck, RecordEvent, EXPERIENCE_MUTATIONS, ROLE_LESSON, TASK_PREFIX,
};
use crate::error::Result;
use crate::store::Store;
use crate::task_memory::{TaskArtifact, TaskSearchFilters};

/// Largest accepted case file, in bytes.
pub const MAX_CASE_BYTES: usize = 32 * 1024;
const SHARED_ID_PREFIX: &str = "agentdash:";

/// A solved problem as an agent writes it. Unknown fields are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseFile {
    pub problem: String,
    pub symptoms: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    pub fix: String,
    pub check: CaseCheck,
    #[serde(default)]
    pub applies_when: BTreeMap<String, String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default = "shared")]
    pub visibility: LessonVisibility,
}

/// How the fix was verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseCheck {
    pub method: CheckMethod,
    pub result: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<String>,
    /// ID of a passing memd check receipt. Only valid with `executed`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub receipt_id: Option<String>,
}

/// Deserialize an optional field that must be left out or given a value.
/// Without this, serde reads an explicit `null` as "absent".
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMethod {
    Observed,
    Executed,
}

fn shared() -> LessonVisibility {
    LessonVisibility::Shared
}

impl CaseFile {
    /// Parse and validate a case file. The size limit applies to the raw bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CASE_BYTES {
            return Err(validation(format!(
                "case file is {} bytes; the limit is 32 KiB",
                bytes.len()
            )));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| validation(format!("invalid case file: {error}")))?;
        reject_nulls(&value, "")?;
        let case: Self = serde_json::from_value(value)
            .map_err(|error| validation(format!("invalid case file: {error}")))?;
        case.validate()?;
        Ok(case)
    }

    /// Check every field against the exchange contract.
    pub fn validate(&self) -> Result<()> {
        text("problem", &self.problem, 300)?;
        items("symptoms", &self.symptoms, 1, 12, 80)?;
        items("failed", &self.failed, 0, 8, 300)?;
        text("fix", &self.fix, 1000)?;
        text("check.result", &self.check.result, 300)?;
        if let Some(command) = &self.check.command {
            text("check.command", command, 300)?;
        }
        if let Some(receipt_id) = &self.check.receipt_id {
            text("check.receipt_id", receipt_id, 80)?;
            if self.check.method != CheckMethod::Executed {
                return Err(validation("check.receipt_id requires method \"executed\""));
            }
        }
        if self.applies_when.len() > 8 {
            return Err(validation("applies_when may hold at most 8 keys"));
        }
        for (key, value) in &self.applies_when {
            if !is_condition_key(key) {
                return Err(validation(format!(
                    "applies_when key '{key}' must match ^[a-z][a-z0-9_]{{0,31}}$"
                )));
            }
            text(&format!("applies_when.{key}"), value, 80)?;
        }
        if self.applies_when.contains_key("version") && !self.applies_when.contains_key("tool") {
            return Err(validation(
                "applies_when.version requires applies_when.tool",
            ));
        }
        items("evidence", &self.evidence, 0, 8, 300)?;
        Ok(())
    }
}

/// An optional field is left out or given a value. An explicit `null` in the
/// case object or its `check` object is an error, like an unknown field, so a
/// file means one thing to every tool that reads it.
fn reject_nulls(value: &serde_json::Value, prefix: &str) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for (key, field) in object {
        if field.is_null() {
            return Err(validation(format!(
                "invalid case file: {prefix}{key} must be left out or have a value, not null"
            )));
        }
        if key == "check" && prefix.is_empty() {
            reject_nulls(field, "check.")?;
        }
    }
    Ok(())
}

fn text(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(validation(format!(
            "{field} must be 1..{max} characters and not blank"
        )));
    }
    Ok(())
}

fn items(field: &str, values: &[String], min: usize, max: usize, each: usize) -> Result<()> {
    if values.len() < min || values.len() > max {
        return Err(validation(format!("{field} must hold {min}..{max} items")));
    }
    values
        .iter()
        .try_for_each(|value| text(&format!("{field} item"), value, each))
}

fn is_condition_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && key.len() <= 32
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_shared_id(id: &str) -> Result<()> {
    let mut bytes = id.bytes();
    let valid = id.len() <= 80
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(validation(
            "shared ID must be 1..80 characters of letters, digits, '.', '_' or '-'",
        ))
    }
}

/// What [`record_case`] stored, or found already stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteOutcome {
    pub problem_id: String,
    pub attempt_ids: Vec<String>,
    pub check_id: String,
    pub lesson_id: String,
    pub evidence: LessonEvidence,
    /// True when the case was already stored and nothing was written.
    pub replay: bool,
    /// Chunk rows the store wrote for this call: one per searchable record,
    /// more when a long lesson is split. A compact case has one searchable
    /// record, the lesson. Zero for a replay.
    pub rows_written: usize,
}

/// Store a case file as problem, attempts, check, and lesson.
///
/// Every input is validated before the first write, and the mutation lock is
/// held across the whole sequence, so concurrent notes cannot interleave.
///
/// The check decides the lesson's evidence label:
/// * `observed`, or `executed` without `receipt_id`: memd saw no command run,
///   so the check is an [`ObservedCheck`] and the lesson reads `observed`.
/// * `executed` with `receipt_id`: the receipt must be a passing check already
///   in this store. The lesson is added to that receipt's problem, which keeps
///   its own attempts, and reads `executed`.
///
/// A call is a replay only when it repeats a stored submission, and then it
/// writes nothing and returns the stored case:
/// * With `shared_id`, the artifacts carry `agentdash:<id>` as provenance, and
///   a stored lesson with the same ID is the same submission. The content is
///   not compared, and a different ID never matches.
/// * Without `shared_id`, a stored lesson in this project with the same
///   canonical content is the same submission: problem, symptoms, fix, check
///   (result and command, or the receipt), and `applies_when`. `failed`,
///   `evidence`, and `visibility` are not part of that content.
///
/// Anything else adds a case, or, for a receipt, a lesson on the receipt's
/// problem.
///
/// With `compact`, every event is still stored, but only the lesson gets a
/// searchable row. The replay rules above read lessons only, so they hold for
/// a compact case and for a case stored in full.
///
/// A crash between the writes leaves an unfinished case: a problem, maybe
/// attempts and a check, but no lesson. The same submission finishes it
/// instead of starting a second case. The lookup reads the artifact tables,
/// because a compact case has no row before its lesson. An unfinished case
/// matches when its problem, symptoms, attempts and check are a prefix of the
/// file's; with `shared_id` it must also carry `agentdash:<id>`. The call
/// reuses the stored records, writes the missing ones, and returns
/// `replay: false` with the rows of this call only. A stored case that does
/// not match (other content under the same ID) is left alone and the call adds
/// a new case, as it did before.
pub async fn record_case<S: Store + ?Sized>(
    store: &S,
    mut context: ExperienceContext,
    case: CaseFile,
    shared_id: Option<&str>,
    compact: bool,
) -> Result<NoteOutcome> {
    case.validate()?;
    if context.project_id.as_option().is_none() {
        return Err(validation(
            "experience cases require an explicit project_id",
        ));
    }
    if let Some(id) = shared_id {
        validate_shared_id(id)?;
        context.provenance.uri = Some(format!("{SHARED_ID_PREFIX}{id}"));
    }

    let _guard = EXPERIENCE_MUTATIONS.lock().await;
    if let Some(lesson) = find_replay(store, &context, &case).await? {
        return outcome_for(store, &context, &lesson, true, 0).await;
    }
    match (case.check.method, case.check.receipt_id.as_deref()) {
        (CheckMethod::Executed, Some(receipt_id)) => {
            attach_to_receipt(store, context, &case, receipt_id).await
        }
        _ => {
            let stored = find_unfinished(store, &context, &case).await?;
            write_case(store, context, &case, compact, stored).await
        }
    }
}

/// The stored records of an unfinished case, in causal order.
#[derive(Default)]
struct Stored {
    problem: Option<TaskArtifact>,
    attempts: Vec<TaskArtifact>,
    check: Option<TaskArtifact>,
}

/// The unfinished case of this submission, or nothing stored (see
/// [`record_case`]). It reads the artifact tables, because a crash can leave a
/// compact case without any chunk row.
async fn find_unfinished<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    case: &CaseFile,
) -> Result<Stored> {
    let mut tasks = store
        .list_tasks(
            &context.tenant_id,
            context.project_id.as_option(),
            EXHAUSTIVE_LIMIT,
        )
        .await?;
    tasks.retain(|task| task.task_id.starts_with(TASK_PREFIX));
    // Stable pick when several unfinished cases match.
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    for task in tasks {
        let mut events = store
            .list_task_artifacts(&context.tenant_id, &task.task_id)
            .await?
            .into_iter()
            .filter(|artifact| artifact.experience.is_some())
            .collect::<Vec<_>>();
        sort_artifacts(&mut events);
        if let Some(stored) = unfinished_prefix(events, context, case) {
            return Ok(stored);
        }
    }
    Ok(Stored::default())
}

/// `events` as a [`Stored`] case when they are a prefix of `case`: a problem,
/// then the first attempts, then a check on the fix. A lesson, a correction,
/// any other record, or any difference from the file gives `None`.
fn unfinished_prefix(
    mut events: Vec<TaskArtifact>,
    context: &ExperienceContext,
    case: &CaseFile,
) -> Option<Stored> {
    let check = match events.last().and_then(|last| last.experience.as_ref()) {
        Some(ExperienceEvent::ObservedCheck(_)) => events.pop(),
        _ => None,
    };
    if events.is_empty() {
        return None;
    }
    let problem = events.remove(0);
    let Some(ExperienceEvent::Problem {
        description,
        symptom_terms,
    }) = problem.experience.as_ref()
    else {
        return None;
    };
    if *description != case.problem
        || *symptom_terms != case.symptoms
        || problem.project_id != context.project_id
    {
        return None;
    }
    if let Some(uri) = context.provenance.uri.as_deref() {
        if problem.provenance.uri.as_deref() != Some(uri) {
            return None;
        }
    }
    let actions = case
        .failed
        .iter()
        .chain(std::iter::once(&case.fix))
        .collect::<Vec<_>>();
    if events.len() > actions.len() {
        return None;
    }
    for (attempt, action) in events.iter().zip(&actions) {
        match attempt.experience.as_ref() {
            Some(ExperienceEvent::Attempt { action: stored, .. }) if stored == *action => {}
            _ => return None,
        }
    }
    if let Some(check) = &check {
        let Some(ExperienceEvent::ObservedCheck(observed)) = check.experience.as_ref() else {
            return None;
        };
        let on_the_fix = events.len() == actions.len()
            && events.last().map(|attempt| attempt.artifact_id.as_str())
                == Some(observed.attempt_id.as_str());
        if !on_the_fix
            || observed.result != case.check.result
            || observed.command != case.check.command
        {
            return None;
        }
    }
    Some(Stored {
        problem: Some(problem),
        attempts: events,
        check,
    })
}

/// Write the records of the case that `stored` does not hold yet.
async fn write_case<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    case: &CaseFile,
    compact: bool,
    stored: Stored,
) -> Result<NoteOutcome> {
    let mut clock = Clock::new(context.observed_at_ms);
    if let Some(last) = stored
        .check
        .iter()
        .chain(stored.attempts.last())
        .chain(stored.problem.as_ref())
        .map(event_time)
        .max()
    {
        // The new records must not predate the stored ones.
        clock.not_before(last + 1);
    }
    let mut rows_written = 0;

    let problem = match stored.problem {
        Some(problem) => problem,
        None => {
            let written = record_unlocked(
                store,
                clock.tick(&context),
                RecordEvent::Problem {
                    description: case.problem.clone(),
                    symptom_terms: case.symptoms.clone(),
                },
                !compact,
            )
            .await?;
            rows_written += written.rows;
            written.artifact
        }
    };

    let mut attempt_ids: Vec<String> = stored
        .attempts
        .iter()
        .map(|attempt| attempt.artifact_id.clone())
        .collect();
    for action in case
        .failed
        .iter()
        .chain(std::iter::once(&case.fix))
        .skip(attempt_ids.len())
    {
        let written = record_unlocked(
            store,
            clock.tick(&context),
            RecordEvent::Attempt {
                problem_id: problem.artifact_id.clone(),
                previous_attempt_id: attempt_ids.last().cloned(),
                action: action.clone(),
            },
            !compact,
        )
        .await?;
        rows_written += written.rows;
        attempt_ids.push(written.artifact.artifact_id);
    }
    let fix_attempt_id = attempt_ids.last().cloned().expect("the fix is an attempt");

    let check = match stored.check {
        Some(check) => check,
        None => {
            let written = record_observed_check_unlocked(
                store,
                clock.tick(&context),
                ObservedCheck {
                    attempt_id: fix_attempt_id,
                    result: case.check.result.clone(),
                    command: case.check.command.clone(),
                },
                !compact,
            )
            .await?;
            rows_written += written.rows;
            written.artifact
        }
    };
    let lesson = record_unlocked(
        store,
        clock.tick(&context),
        RecordEvent::Lesson(lesson_from_case(
            case,
            &problem.artifact_id,
            &check.artifact_id,
            LessonEvidence::Observed,
        )),
        true,
    )
    .await?;
    Ok(NoteOutcome {
        problem_id: problem.artifact_id,
        attempt_ids,
        check_id: check.artifact_id,
        lesson_id: lesson.artifact.artifact_id,
        evidence: LessonEvidence::Observed,
        replay: false,
        rows_written: rows_written + lesson.rows,
    })
}

async fn attach_to_receipt<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    case: &CaseFile,
    receipt_id: &str,
) -> Result<NoteOutcome> {
    let check = get_experience(store, &context.tenant_id, receipt_id)
        .await?
        .ok_or_else(|| validation(format!("receipt '{receipt_id}' does not exist")))?;
    let ExperienceEvent::Check(receipt) = required_event(&check)? else {
        return Err(validation(format!(
            "artifact '{receipt_id}' is not an executed check receipt"
        )));
    };
    if !receipt.supports_lesson() {
        return Err(validation(
            "receipt must have exit_code 0, no timeout, complete output hashes, and unchanged source evidence",
        ));
    }
    ensure_project(&context.project_id, &check.project_id)?;
    let attempt = get_experience(store, &context.tenant_id, &receipt.attempt_id)
        .await?
        .ok_or_else(|| validation(format!("attempt '{}' does not exist", receipt.attempt_id)))?;
    let ExperienceEvent::Attempt { problem_id, .. } = required_event(&attempt)? else {
        return Err(validation(format!(
            "receipt '{receipt_id}' does not check an attempt"
        )));
    };

    let mut clock = Clock::new(context.observed_at_ms);
    let lesson = record_unlocked(
        store,
        clock.tick(&context),
        RecordEvent::Lesson(lesson_from_case(
            case,
            problem_id,
            receipt_id,
            LessonEvidence::Executed,
        )),
        true,
    )
    .await?;
    outcome_for(store, &context, &lesson.artifact, false, lesson.rows).await
}

fn lesson_from_case(
    case: &CaseFile,
    problem_id: &str,
    check_id: &str,
    evidence: LessonEvidence,
) -> ConditionalLesson {
    let mut context = case.applies_when.clone();
    let applicability = LessonApplicability {
        machine: context.remove("machine"),
        tool: context.remove("tool"),
        version: context.remove("version"),
    };
    ConditionalLesson {
        problem_id: problem_id.to_string(),
        supporting_check_id: check_id.to_string(),
        guidance: case.fix.clone(),
        symptom_terms: case.symptoms.clone(),
        applicability,
        visibility: case.visibility,
        evidence,
        context,
        links: case.evidence.clone(),
        conflicts_with: Vec::new(),
    }
}

/// The stored lesson this call repeats, if any (see [`record_case`]).
async fn find_replay<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    case: &CaseFile,
) -> Result<Option<TaskArtifact>> {
    let filters = TaskSearchFilters {
        artifact_role: Some(ROLE_LESSON.to_string()),
        project_id: context.project_id.as_option().map(str::to_string),
        ..Default::default()
    };
    let chunk_ids = store
        .search_task_projection_chunk_ids(&context.tenant_id, &filters, EXHAUSTIVE_LIMIT)
        .await?;
    let mut lessons = store
        .resolve_artifacts_for_chunks(&context.tenant_id, &chunk_ids)
        .await?
        .into_values()
        .filter(|artifact| artifact.project_id == context.project_id)
        .collect::<Vec<_>>();
    // Stable pick when several stored lessons match.
    lessons.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));

    if let Some(uri) = context.provenance.uri.as_deref() {
        return Ok(lessons
            .into_iter()
            .find(|artifact| artifact.provenance.uri.as_deref() == Some(uri)));
    }
    for artifact in lessons {
        if has_same_content(store, context, case, &artifact).await? {
            return Ok(Some(artifact));
        }
    }
    Ok(None)
}

/// Compare a stored lesson with the canonical content of `case`. Cheap fields
/// come first, so the problem and the check load only for a close match.
async fn has_same_content<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    case: &CaseFile,
    artifact: &TaskArtifact,
) -> Result<bool> {
    let Some(lesson) = artifact.experience.as_ref().and_then(lesson_from_event) else {
        return Ok(false);
    };
    if lesson.guidance != case.fix
        || lesson.symptom_terms != case.symptoms
        || applies_when_of(lesson) != case.applies_when
    {
        return Ok(false);
    }
    let receipt_id = case
        .check
        .receipt_id
        .as_deref()
        .filter(|_| case.check.method == CheckMethod::Executed);
    if let Some(receipt_id) = receipt_id {
        // The lesson sits on the receipt's own problem, which the file does
        // not describe, so the receipt is the whole check and problem match.
        return Ok(lesson.supporting_check_id == receipt_id);
    }
    let Some(check) =
        get_experience(store, &context.tenant_id, &lesson.supporting_check_id).await?
    else {
        return Ok(false);
    };
    let Some(ExperienceEvent::ObservedCheck(observed)) = check.experience.as_ref() else {
        return Ok(false);
    };
    if observed.result != case.check.result || observed.command != case.check.command {
        return Ok(false);
    }
    let problem = get_experience(store, &context.tenant_id, &lesson.problem_id).await?;
    Ok(matches!(
        problem.as_ref().and_then(|problem| problem.experience.as_ref()),
        Some(ExperienceEvent::Problem { description, .. }) if *description == case.problem
    ))
}

/// The `applies_when` object a lesson was built from.
fn applies_when_of(lesson: &ConditionalLesson) -> BTreeMap<String, String> {
    let mut conditions = lesson.context.clone();
    for (key, value) in [
        ("machine", &lesson.applicability.machine),
        ("tool", &lesson.applicability.tool),
        ("version", &lesson.applicability.version),
    ] {
        if let Some(value) = value {
            conditions.insert(key.to_string(), value.clone());
        }
    }
    conditions
}

async fn outcome_for<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    lesson_artifact: &TaskArtifact,
    replay: bool,
    rows_written: usize,
) -> Result<NoteOutcome> {
    let lesson = lesson_from_event(required_event(lesson_artifact)?)
        .ok_or_else(|| validation("stored artifact is not a lesson"))?;
    let stored = resolve_case(store, &context.tenant_id, &lesson.problem_id).await?;
    let attempt_ids = stored
        .events
        .iter()
        .filter(|artifact| matches!(artifact.experience, Some(ExperienceEvent::Attempt { .. })))
        .map(|artifact| artifact.artifact_id.clone())
        .collect();
    Ok(NoteOutcome {
        problem_id: lesson.problem_id.clone(),
        attempt_ids,
        check_id: lesson.supporting_check_id.clone(),
        lesson_id: lesson_artifact.artifact_id.clone(),
        evidence: lesson.evidence,
        replay,
        rows_written,
    })
}

/// Hands out strictly increasing observation times, so the records of one
/// case order by cause even when they land in the same millisecond.
struct Clock(i64);

impl Clock {
    fn new(requested: Option<i64>) -> Self {
        Self(requested.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or(0)
        }))
    }

    /// Never hand out a time before `earliest`.
    fn not_before(&mut self, earliest: i64) {
        self.0 = self.0.max(earliest);
    }

    fn tick(&mut self, context: &ExperienceContext) -> ExperienceContext {
        let mut stamped = context.clone();
        stamped.observed_at_ms = Some(self.0);
        self.0 += 1;
        stamped
    }
}

#[cfg(test)]
mod finish_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "problem": "p",
            "symptoms": ["s"],
            "fix": "f",
            "check": {"method": "observed", "result": "r"}
        })
    }

    #[test]
    fn defaults_fill_in_and_visibility_is_shared() {
        let case = CaseFile::parse(minimal().to_string().as_bytes()).unwrap();
        assert!(case.failed.is_empty() && case.evidence.is_empty());
        assert!(case.applies_when.is_empty());
        assert_eq!(case.visibility, LessonVisibility::Shared);
    }

    #[test]
    fn a_null_optional_is_an_error_for_every_reader() {
        // The struct rejects null itself, so a case that arrives as arguments
        // to `experience.note` is held to the same rule as a parsed file.
        let mut value = minimal();
        value["check"]["command"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<CaseFile>(value.clone()).is_err());
        assert!(CaseFile::parse(value.to_string().as_bytes()).is_err());
        value["check"]["command"] = serde_json::json!("run it");
        value["check"]["receipt_id"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<CaseFile>(value).is_err());
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        let mut value = minimal();
        value["symptoms"] = serde_json::json!(["é".repeat(80)]);
        assert!(CaseFile::parse(value.to_string().as_bytes()).is_ok());
        value["symptoms"] = serde_json::json!(["é".repeat(81)]);
        assert!(CaseFile::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn condition_keys_follow_the_contract_pattern() {
        for good in ["a", "tool", "os_2", "a".repeat(32).as_str()] {
            assert!(is_condition_key(good), "{good}");
        }
        for bad in ["", "Tool", "2os", "a-b", "a".repeat(33).as_str()] {
            assert!(!is_condition_key(bad), "{bad}");
        }
    }

    #[test]
    fn shared_ids_are_path_safe() {
        assert!(validate_shared_id("EXP-17").is_ok());
        assert!(validate_shared_id("").is_err());
        assert!(validate_shared_id("a b").is_err());
        assert!(validate_shared_id("-lead").is_err());
        assert!(validate_shared_id(&"x".repeat(81)).is_err());
    }
}
