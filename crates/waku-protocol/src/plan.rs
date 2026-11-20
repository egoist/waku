//! Reading a provider's plan out of the payload its plan tool carried.
//!
//! Every provider that plans writes its steps into a tool call, but each
//! writes them under its own key and in its own shape: Claude Code sends
//! `TodoWrite` with a `todos` list, a `todo/write` event carries the list
//! itself, a plan item carries `entries`. The payload is therefore searched
//! for the first list whose steps can be read, and each step is read by what
//! it says and by the state it is in.

use serde_json::Value;

use crate::model::{ActivityPlan, PlanEntry, PlanEntryStatus};

/// The keys a provider writes its plan steps under.
const ENTRY_KEYS: &[&str] = &["todos", "plan", "entries", "items", "steps", "tasks"];

/// The keys a plan step's own text can sit under.
const TEXT_KEYS: &[&str] = &[
    "content",
    "step",
    "text",
    "title",
    "description",
    "name",
    "activeForm",
];

/// The keys a provider nests its payload under.
const PAYLOAD_KEYS: &[&str] = &[
    "input",
    "arguments",
    "args",
    "params",
    "toolInput",
    "tool_input",
    "rawInput",
    "raw_input",
];

/// Reads the plan out of a plan tool's payload.
pub fn extract_plan_from_value(value: &Value) -> Option<ActivityPlan> {
    let entries = find_entries(value, 0)?;
    (!entries.is_empty()).then_some(ActivityPlan { entries })
}

/// Finds the list of steps in a plan payload.
fn find_entries(value: &Value, depth: usize) -> Option<Vec<PlanEntry>> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::Array(values) => {
            let entries: Vec<PlanEntry> = values.iter().filter_map(entry).collect();
            (!entries.is_empty()).then_some(entries)
        }
        Value::Object(object) => {
            for key in ENTRY_KEYS {
                let Some(payload) = object.get(*key) else {
                    continue;
                };
                let found = match payload {
                    Value::Array(values) => {
                        let entries: Vec<PlanEntry> = values.iter().filter_map(entry).collect();
                        (!entries.is_empty()).then_some(entries)
                    }
                    other => find_entries(other, depth + 1),
                };
                if let Some(entries) = found {
                    return Some(entries);
                }
            }
            for key in PAYLOAD_KEYS {
                if let Some(entries) = object
                    .get(*key)
                    .and_then(|value| find_entries(value, depth + 1))
                {
                    return Some(entries);
                }
            }
            None
        }
        _ => None,
    }
}

/// Reads one step of a plan.
fn entry(value: &Value) -> Option<PlanEntry> {
    let object = value.as_object()?;
    let content = TEXT_KEYS.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })?;
    Some(PlanEntry {
        content,
        status: status(object),
    })
}

/// Reads the state of a step.
///
/// Providers spell the states differently, so the words are matched loosely:
/// what matters is that a step reads as waiting, under way, finished, or
/// dropped.
fn status(object: &serde_json::Map<String, Value>) -> PlanEntryStatus {
    for key in ["status", "state"] {
        let Some(status) = object.get(key).and_then(Value::as_str) else {
            continue;
        };
        return match status.trim().to_ascii_lowercase().as_str() {
            "in_progress" | "inprogress" | "in-progress" | "active" | "running" | "doing" => {
                PlanEntryStatus::InProgress
            }
            "completed" | "complete" | "done" | "finished" | "resolved" => {
                PlanEntryStatus::Completed
            }
            "cancelled" | "canceled" | "skipped" | "abandoned" | "failed" => {
                PlanEntryStatus::Cancelled
            }
            _ => PlanEntryStatus::Pending,
        };
    }
    match object.get("completed").and_then(Value::as_bool) {
        Some(true) => PlanEntryStatus::Completed,
        _ => PlanEntryStatus::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_claude_code_todos() {
        let plan = extract_plan_from_value(&json!({
            "todos": [
                {"content": "Fix it", "status": "in_progress", "activeForm": "Fixing it"},
                {"content": "Ship it", "status": "pending"}
            ]
        }))
        .expect("plan");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Fix it");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
        assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
    }

    #[test]
    fn reads_a_list_of_steps() {
        let plan = extract_plan_from_value(&json!([
            {"content": "One", "status": "completed"},
            {"content": "Two", "status": "cancelled"}
        ]))
        .expect("plan");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].status, PlanEntryStatus::Cancelled);
    }

    #[test]
    fn reads_a_nested_plan() {
        let plan = extract_plan_from_value(&json!({
            "input": {"plan": {"entries": [{"step": "One", "status": "done"}]}}
        }))
        .expect("plan");
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "One");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
    }

    #[test]
    fn reads_a_completed_flag() {
        let plan = extract_plan_from_value(&json!({"items": [{"text": "One", "completed": true}]}))
            .expect("plan");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
    }

    #[test]
    fn a_payload_without_steps_has_no_plan() {
        assert!(extract_plan_from_value(&json!({"command": "ls"})).is_none());
        assert!(extract_plan_from_value(&json!([])).is_none());
    }

    #[test]
    fn a_plan_tool_carries_its_steps() {
        use crate::model::{ActivityItem, ActivityKind};
        let item = ActivityItem::new(None, ActivityKind::Plan, "TodoWrite", None, true)
            .with_arguments(Some(
                r#"{"todos": [{"content": "Fix it", "status": "in_progress"}]}"#.to_owned(),
            ));
        let plan = item.plan.expect("plan");
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "Fix it");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn a_plan_reported_as_output_carries_its_steps() {
        use crate::model::{ActivityItem, ActivityKind};
        let item =
            ActivityItem::new(None, ActivityKind::Plan, "todo", None, true).with_output(Some(
                r#"[{"content": "Read the issue", "status": "completed"},
                {"content": "Fix it", "status": "in_progress"}]"#
                    .to_owned(),
            ));
        let plan = item.plan.expect("plan");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn a_non_plan_tool_carries_no_plan() {
        use crate::model::{ActivityItem, ActivityKind};
        let item = ActivityItem::new(None, ActivityKind::Command, "bash", None, true)
            .with_arguments(Some(r#"{"todos": [{"content": "Not a plan"}]}"#.to_owned()));
        assert!(item.plan.is_none());
    }
}
