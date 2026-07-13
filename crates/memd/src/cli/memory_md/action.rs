pub(in crate::cli) fn explicit_agent_action(text: &str) -> Option<String> {
    const MARKERS: &[(&str, &str)] = &[
        ("agent action:", ""),
        ("action:", ""),
        ("rule:", ""),
        ("do:", "Do "),
        ("use:", "Use "),
        ("avoid:", "Avoid "),
        ("prefer:", "Prefer "),
        ("check:", "Check "),
        ("verify:", "Verify "),
        ("next step:", "Do next: "),
        ("follow-up:", "Follow up: "),
        ("followup:", "Follow up: "),
    ];

    for marker in explicit_action_markers(text, MARKERS) {
        let body = explicit_action_body(text, marker.start, marker.marker);
        if body.is_empty() {
            continue;
        }
        let candidate = format!("{}{}", marker.prefix, body);
        if is_concrete_agent_action_text(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ExplicitActionMarker<'a> {
    pub(super) start: usize,
    pub(super) marker: &'a str,
    pub(super) prefix: &'a str,
}

pub(super) fn explicit_action_markers<'a>(
    text: &str,
    markers: &'a [(&'a str, &'a str)],
) -> Vec<ExplicitActionMarker<'a>> {
    let lowered = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for (marker, prefix) in markers {
        let mut search_start = 0;
        while let Some(relative_start) = lowered[search_start..].find(marker) {
            let start = search_start + relative_start;
            found.push(ExplicitActionMarker {
                start,
                marker,
                prefix,
            });
            search_start = start + marker.len();
        }
    }
    found.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.marker.len().cmp(&left.marker.len()))
    });
    found
}

pub(super) fn explicit_action_body<'a>(
    text: &'a str,
    marker_start: usize,
    marker: &str,
) -> &'a str {
    let body_start = marker_start + marker.len();
    let lowered_tail = text[body_start..].to_ascii_lowercase();
    let line_end = text[body_start..]
        .find(['\n', '\r'])
        .map(|offset| body_start + offset)
        .unwrap_or(text.len());
    let next_marker = [
        "agent action:",
        "action:",
        "rule:",
        "do:",
        "use:",
        "avoid:",
        "prefer:",
        "check:",
        "verify:",
        "next step:",
        "follow-up:",
        "followup:",
    ]
    .iter()
    .filter_map(|candidate| {
        lowered_tail
            .find(candidate)
            .map(|offset| body_start + offset)
    })
    .min()
    .unwrap_or(text.len());
    let body_end = line_end.min(next_marker);
    text[body_start..body_end]
        .trim()
        .trim_end_matches(['.', ';'])
        .trim_end()
}

pub(super) fn inline_code_text(text: &str) -> String {
    text.replace('`', "'")
}

pub(super) fn is_concrete_agent_action_text(action: &str) -> bool {
    action.chars().count() >= 24 && contains_action_verb(action)
}

pub(super) fn contains_action_verb(text: &str) -> bool {
    // Shared with the write-admission gate so renderer and gate agree.
    text.split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| {
            crate::write_admission::ACTION_VERBS.contains(&word.to_ascii_lowercase().as_str())
        })
}

pub(super) fn summarize_text(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let mut out = collapsed
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}
