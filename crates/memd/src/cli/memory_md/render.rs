use super::action::*;
use super::rank::*;
use super::state::*;
use super::*;

pub(super) fn render_memory_md(
    project_state: &ProjectState,
    health_lines: &[String],
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> String {
    let mut out = String::new();
    out.push_str("# memory.md\n\n");
    render_latest_project_state(&mut out, project_state);
    let warnings = project_state_warnings(project_state);
    if !health_lines.is_empty() || !warnings.is_empty() {
        out.push_str("## Memory health\n\n");
        for line in health_lines {
            out.push_str(&format!("- {line}\n"));
        }
        for warning in &warnings {
            out.push_str(&format!("- {warning}\n"));
        }
        out.push('\n');
    }

    render_section(&mut out, "Project Fact Library", project_takeaways);
    if !global_takeaways.is_empty() {
        render_section(&mut out, "Machine-Wide Fact Library", global_takeaways);
    }
    out
}

pub(super) fn render_latest_project_state(out: &mut String, state: &ProjectState) {
    out.push_str(&format!(
        "Generated {} | tenant {} | project {}\n\n",
        crate::ops::format_epoch_ms_date(state.generated_unix_ms as i64),
        state.tenant_id,
        state.project_id.as_deref().unwrap_or("<none>")
    ));
}

pub(super) fn project_state_warnings(state: &ProjectState) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(state.scope_warnings.iter().cloned());
    if let Some(unreadable) = state.memory.unreadable_active_chunks {
        if unreadable > 0 {
            warnings.push(format!(
                "memory degraded: {unreadable} active chunks could not be read from payload segments"
            ));
        }
    }
    if let Some(warning) = &state.memory.scan_warning {
        warnings.push(warning.clone());
    }
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
        takeaway.chunk_id, takeaway.priority
    ));
    if !takeaway.tags.is_empty() {
        out.push_str(&format!(
            "; tags: `{}`",
            takeaway
                .tags
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push('\n');
}
