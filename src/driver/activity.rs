use std::collections::HashSet;

use serde_json::Value;

use crate::model::{ActivityItem, ActivityKind};

const MAX_ACTIVITY_CHARS: usize = 16_000;

pub(super) fn tool_activity(
    source_id: Option<String>,
    kind: ActivityKind,
    title: String,
    arguments: Option<&Value>,
    output: Option<&Value>,
    image_source: Option<&Value>,
    failed: bool,
    complete: bool,
) -> ActivityItem {
    let arguments = arguments
        .filter(|value| !value.is_null())
        .and_then(format_json);
    let formatted_output = output
        .filter(|value| !value.is_null())
        .and_then(format_output);
    let mut image_urls = Vec::new();
    if let Some(value) = output {
        collect_image_urls(value, &mut image_urls);
    }
    if let Some(value) = image_source {
        collect_image_urls(value, &mut image_urls);
    }
    let mut seen = HashSet::new();
    image_urls.retain(|url| seen.insert(url.clone()));
    let detail = failed
        .then(|| {
            formatted_output.as_deref()?.lines().find_map(|line| {
                let line = line.trim();
                (!line.is_empty()).then(|| line.to_owned())
            })
        })
        .flatten();

    ActivityItem::new(source_id, kind, title, detail, complete)
        .with_arguments(arguments)
        .with_output(formatted_output)
        .with_image_urls(image_urls)
        .with_failed(failed)
}

pub(super) fn input_title(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("title")
        .or_else(|| value.pointer("/arguments/title"))
        .or_else(|| value.pointer("/input/title"))
        .or_else(|| value.pointer("/tool_input/title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_supports_direct_and_provider_wrapped_arguments() {
        assert_eq!(
            input_title(Some(&serde_json::json!({"title": "Inspect app"}))).as_deref(),
            Some("Inspect app")
        );
        assert_eq!(
            input_title(Some(&serde_json::json!({
                "tool_name": "waku_js_repl__js",
                "arguments": {"code": "1", "title": "Inspect wrapped app"}
            })))
            .as_deref(),
            Some("Inspect wrapped app")
        );
        assert_eq!(
            input_title(Some(&serde_json::json!({
                "tool_name": "waku_js_repl__js",
                "tool_input": {"code": "1", "title": "Verify Grok bridge"}
            })))
            .as_deref(),
            Some("Verify Grok bridge")
        );
    }
}

pub(super) fn format_json(value: &Value) -> Option<String> {
    serde_json::to_string_pretty(value)
        .ok()
        .and_then(non_empty_text)
}

fn format_output(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return non_empty_text(text.to_owned());
    }
    if let Some(structured) = value
        .get("structuredContent")
        .filter(|value| !value.is_null())
    {
        return format_json(structured);
    }
    if let Some(content) = value.get("content").filter(|value| !value.is_null()) {
        return format_output(content);
    }
    if let Some(items) = value.as_array() {
        let text = items
            .iter()
            .filter(|item| !is_image_item(item))
            .filter_map(|item| {
                item.as_str().map(str::to_owned).or_else(|| {
                    (item.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| item.get("text").and_then(Value::as_str).map(str::to_owned))
                        .flatten()
                        .or_else(|| format_json(item))
                })
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return non_empty_text(text);
    }
    format_json(value)
}

fn collect_image_urls(value: &Value, urls: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_image_urls(item, urls);
            }
        }
        Value::Object(object) => {
            if is_image_item(value) {
                if let Some(url) = object
                    .get("imageUrl")
                    .or_else(|| object.get("image_url"))
                    .or_else(|| object.get("url"))
                    .and_then(Value::as_str)
                {
                    urls.push(url.to_owned());
                } else if let Some(data) = object.get("data").and_then(Value::as_str) {
                    let mime = object
                        .get("mime")
                        .or_else(|| object.get("mimeType"))
                        .or_else(|| object.get("mime_type"))
                        .or_else(|| {
                            object
                                .get("source")
                                .and_then(|source| source.get("media_type"))
                        })
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    urls.push(format!("data:{mime};base64,{data}"));
                } else if let Some(data) = object
                    .get("source")
                    .and_then(|source| source.get("data"))
                    .and_then(Value::as_str)
                {
                    let mime = object
                        .get("source")
                        .and_then(|source| source.get("media_type"))
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    urls.push(format!("data:{mime};base64,{data}"));
                }
            }
            for key in ["content", "attachments", "files", "result"] {
                if let Some(nested) = object.get(key) {
                    collect_image_urls(nested, urls);
                }
            }
        }
        _ => {}
    }
}

fn is_image_item(value: &Value) -> bool {
    let item_type = value.get("type").and_then(Value::as_str);
    let mime = value
        .get("mime")
        .or_else(|| value.get("mimeType"))
        .or_else(|| value.get("mime_type"))
        .or_else(|| value.pointer("/source/media_type"))
        .and_then(Value::as_str);
    matches!(item_type, Some("image" | "inputImage"))
        || (item_type == Some("file") && mime.is_some_and(|mime| mime.starts_with("image/")))
}

fn non_empty_text(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return None;
    }
    if value.chars().count() <= MAX_ACTIVITY_CHARS {
        return Some(value);
    }
    let mut truncated = value.chars().take(MAX_ACTIVITY_CHARS).collect::<String>();
    truncated.push_str("\n\n… output truncated");
    Some(truncated)
}
