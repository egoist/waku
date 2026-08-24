/// Parse the leading YAML frontmatter block used by commands and skills.
///
/// Unknown and unsupported values are ignored so a hand-written prompt still
/// stays listed. YAML syntax, including folded and literal block scalars, is
/// handled by `serde-saphyr`.
pub(crate) fn parse_frontmatter_fields<'a>(
    contents: &'a str,
    mut visit: impl FnMut(&str, String),
) -> &'a str {
    let Some(rest) = contents.strip_prefix("---") else {
        return contents;
    };
    let Some((block, body)) = rest.split_once("\n---") else {
        return contents;
    };

    if let Ok(fields) = serde_saphyr::from_str::<serde_json::Map<String, serde_json::Value>>(block)
    {
        for (key, value) in fields {
            if let Some(value) = frontmatter_value(value) {
                visit(&key, value);
            }
        }
    }

    body.trim_start_matches(['-']).trim_start()
}

fn frontmatter_value(value: serde_json::Value) -> Option<String> {
    let value = match value {
        serde_json::Value::String(value) => value,
        // Preserve the bracket notation historically accepted by command
        // metadata such as `argument-hint: [pr-number]`.
        serde_json::Value::Array(values) => {
            let values = values
                .into_iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?;
            format!("[{}]", values.join(", "))
        }
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Null | serde_json::Value::Object(_) => return None,
    };
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
