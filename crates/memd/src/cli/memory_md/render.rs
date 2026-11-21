use super::action::*;
use super::rank::*;
use super::state::*;
use super::*;
use crate::experience::{LessonEvidence, RecentLesson};

pub(super) fn render_memory_md(
    project_state: &ProjectState,
    project_takeaways: &[Takeaway],
    lessons: &[&RecentLesson],
    global_takeaways: &[Takeaway],
) -> String {
    let mut out = String::new();
    out.push_str("# memory.md\n\n");
    render_latest_project_state(&mut out, project_state);
    out.push_str("## Memory health\n\n");
    out.push_str(&format!("- {}\n", facts_health_line(project_state)));
    for warning in project_state_warnings(project_state) {
        out.push_str(&format!("- {warning}\n"));
    }
    out.push('\n');

    render_section(&mut out, "Project Fact Library", project_takeaways);
    render_lessons(&mut out, lessons);
    if !global_takeaways.is_empty() {
        render_section(&mut out, "Machine-Wide Fact Library", global_takeaways);
    }
    out
}

/// Length limits for untrusted fields.
const LESSON_GUIDANCE_CHARS: usize = 320;
const LESSON_TERM_CHARS: usize = 80;
const LESSON_TERMS_SHOWN: usize = 12;
const LESSON_ENVIRONMENT_CHARS: usize = 60;
const TAG_CHARS: usize = 60;
const ID_CHARS: usize = 80;

/// Untrusted text (lesson fields, tags) as inert single-line text. Agents
/// read `memory.md` as context, so a stored value must not start a heading,
/// list item, quote, code fence or HTML tag, or close a code span. One rule
/// for every such field:
/// * whitespace, control characters and bidirectional-format characters
///   become one space, so the text stays on one line;
/// * backticks become apostrophes, so the text cannot close a code span;
/// * `<` and `>` become single angle quotes, so no tag can form;
/// * a leading run of `#`, `>`, `-`, `*`, `+`, `=`, `_`, `~`, `|` is dropped;
/// * the result is cut to `limit` characters, with `...` when cut.
pub(super) fn inert_text(text: &str, limit: usize) -> String {
    let mut single_line = String::with_capacity(text.len().min(limit * 4));
    let mut pending_space = false;
    for ch in text.chars() {
        let ch = match ch {
            '`' => '\'',
            '<' => '\u{2039}',
            '>' => '\u{203a}',
            ch => ch,
        };
        if ch.is_whitespace()
            || ch.is_control()
            || matches!(ch, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        {
            pending_space = !single_line.is_empty();
            continue;
        }
        if pending_space {
            single_line.push(' ');
            pending_space = false;
        }
        single_line.push(ch);
    }
    let trimmed = single_line
        .trim_start_matches(['#', '\u{203a}', '-', '*', '+', '=', '_', '~', '|', ' '])
        .to_string();
    summarize_text(&trimmed, limit)
}

/// The shown lessons of the project, each rendered from the lesson itself:
/// guidance, then when it applies and how it was checked. Nothing renders
/// without a lesson. Every field passes through [`inert_text`].
pub(super) fn render_lessons(out: &mut String, lessons: &[&RecentLesson]) {
    if lessons.is_empty() {
        return;
    }
    out.push_str("## Recent Lessons\n\n");
    for (index, recent) in lessons.iter().enumerate() {
        let lesson = &recent.lesson;
        out.push_str(&format!(
            "{}. {}\n",
            index + 1,
            inert_text(&lesson.guidance, LESSON_GUIDANCE_CHARS)
        ));
        let mut parts = Vec::new();
        let terms = lesson
            .symptom_terms
            .iter()
            .take(LESSON_TERMS_SHOWN)
            .map(|term| inert_text(term, LESSON_TERM_CHARS))
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        if !terms.is_empty() {
            parts.push(format!("when: {}", terms.join(", ")));
        }
        let applicability = &lesson.applicability;
        let environment = |value: &Option<String>| {
            value
                .as_deref()
                .map(|value| inert_text(value, LESSON_ENVIRONMENT_CHARS))
                .filter(|value| !value.is_empty())
        };
        let mut applies = Vec::new();
        match (
            environment(&applicability.tool),
            environment(&applicability.version),
        ) {
            (Some(tool), Some(version)) => applies.push(format!("tool {tool} {version}")),
            (Some(tool), None) => applies.push(format!("tool {tool}")),
            (None, Some(version)) => applies.push(format!("version {version}")),
            (None, None) => {}
        }
        if let Some(machine) = environment(&applicability.machine) {
            applies.push(format!("machine {machine}"));
        }
        if !applies.is_empty() {
            parts.push(format!("applies to: {}", applies.join(", ")));
        }
        parts.push(format!(
            "evidence: {}",
            match lesson.evidence {
                LessonEvidence::Observed => "observed",
                LessonEvidence::Executed => "executed",
            }
        ));
        parts.push(format!(
            "case: `{}`",
            inert_text(&recent.problem_id, ID_CHARS)
        ));
        out.push_str(&format!("   - {}\n", parts.join("; ")));
    }
    out.push_str("\nMore lessons: memd experience find \"<error words>\"\n\n");
}

pub(super) fn render_latest_project_state(out: &mut String, state: &ProjectState) {
    out.push_str(&format!(
        "Generated {} | tenant {} | project {}\n\n",
        crate::ops::format_epoch_ms_date(state.generated_unix_ms as i64),
        state.tenant_id,
        state.project_id.as_deref().unwrap_or("<none>")
    ));
}

/// The visible-fact counts, from SQL. Without an active project there is no
/// project count to show.
pub(super) fn facts_health_line(state: &ProjectState) -> String {
    let memory = &state.memory;
    match state.project_id.as_deref() {
        Some(project) => format!(
            "facts: {} visible in project {project}, {} visible in tenant {}",
            memory.visible_project_facts, memory.visible_tenant_facts, state.tenant_id
        ),
        None => format!(
            "facts: {} visible in tenant {}",
            memory.visible_tenant_facts, state.tenant_id
        ),
    }
}

pub(super) fn project_state_warnings(state: &ProjectState) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(state.scope_warnings.iter().cloned());
    if state.memory.unreadable_active_chunks > 0 {
        warnings.push(format!(
            "memory degraded: {} active chunks could not be read from payload segments",
            state.memory.unreadable_active_chunks
        ));
    }
    warnings.extend(state.memory.scan_warnings.iter().cloned());
    warnings.extend(state.collection_warnings.iter().cloned());
    warnings
}

pub(super) fn render_section(out: &mut String, title: &str, takeaways: &[Takeaway]) {
    out.push_str(&format!("## {title}\n\n"));
    if takeaways.is_empty() {
        out.push_str("- No takeaways found yet.\n\n");
        return;
    }

    let mut categorized = takeaways
        .iter()
        .map(|takeaway| (takeaway_category(takeaway), takeaway))
        .collect::<Vec<_>>();
    categorized.sort_by(|(left_category, left), (right_category, right)| {
        left_category
            .order
            .cmp(&right_category.order)
            .then_with(|| {
                right
                    .priority
                    .partial_cmp(&left.priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });

    for (heading, _) in TAKEAWAY_CATEGORIES {
        let group = categorized
            .iter()
            .filter(|(category, _)| category.heading == *heading)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("### {heading}\n\n"));
        for (idx, (_, takeaway)) in group.iter().enumerate() {
            render_takeaway(out, idx + 1, takeaway);
        }
        out.push('\n');
    }
}

pub(super) fn render_takeaway(out: &mut String, idx: usize, takeaway: &Takeaway) {
    out.push_str(&format!(
        "{}. {}\n",
        idx,
        summarize_text(&takeaway.text, 320)
    ));
    out.push_str(&format!(
        "   - chunk: `{}`; priority: `{:.1}`",
        inert_text(&takeaway.chunk_id, ID_CHARS),
        takeaway.priority
    ));
    if !takeaway.tags.is_empty() {
        out.push_str(&format!(
            "; tags: `{}`",
            takeaway
                .tags
                .iter()
                .take(4)
                .map(|tag| inert_text(tag, TAG_CHARS))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push('\n');
}
