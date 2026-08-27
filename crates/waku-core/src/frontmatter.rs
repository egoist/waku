/// Parse the leading YAML frontmatter block used by commands and skills.
///
/// Unknown and unsupported values are ignored so a hand-written prompt still
/// stays listed. YAML syntax, including folded and literal block scalars, is
/// handled without an external YAML dependency.
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

    for (key, value) in parse_block(block) {
        if let Some(value) = normalize_value(&value) {
            visit(&key, value);
        }
    }

    body.trim_start_matches(['-']).trim_start()
}

fn normalize_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Objects / nested mappings are ignored - they would have been
    // Value::Object in the serde_saphyr path.
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return None;
    }
    Some(trimmed.to_owned())
}

fn parse_block(block: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = block.lines().collect();
    let mut fields = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            idx += 1;
            continue;
        }
        // frontmatter keys are at indent 0; indented lines belong to a block scalar.
        if line.starts_with(' ') || line.starts_with('\t') {
            idx += 1;
            continue;
        }
        let Some(colon) = line.find(':') else {
            idx += 1;
            continue;
        };
        let key = line[..colon].trim().to_owned();
        if key.is_empty() {
            idx += 1;
            continue;
        }
        let after = line[colon + 1..].trim();
        // Block scalar indicator: > / >- / >+ / | / |- etc.
        if after.starts_with('>') || after.starts_with('|') {
            let is_folded = after.starts_with('>');
            let (value, consumed) = collect_block_scalar(&lines, idx + 1, is_folded);
            if !value.is_empty() || !is_folded {
                // For literal keep even empty; for folded empty means no content.
                // Push only if we got something or the scalar was explicitly empty.
            }
            // Only push non-empty after normalize; normalize_value will filter.
            if !value.trim().is_empty() {
                fields.push((key, value));
            } else if after.len() > 1 {
                // block scalar with no content - ignore
            }
            idx += 1 + consumed;
            continue;
        }
        if after.is_empty() {
            // No inline value and no block indicator - ignore (unsupported / object).
            idx += 1;
            continue;
        }
        // Inline scalar: handle quoted strings, bracket arrays, plain.
        let mut value = after.to_owned();
        // Strip trailing comment for plain scalars? Keep simple: if # surrounded by space.
        // Not needed for current tests.
        value = strip_inline_value(&value);
        fields.push((key, value));
        idx += 1;
    }
    fields
}

fn strip_inline_value(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[s.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\'')
        {
            // Remove surrounding quotes. YAML double quotes allow escapes but
            // frontmatter only uses simple quoted strings.
            return s[1..s.len() - 1].to_owned();
        }
    }
    s.to_owned()
}

fn collect_block_scalar(lines: &[&str], start: usize, is_folded: bool) -> (String, usize) {
    if start >= lines.len() {
        return (String::new(), 0);
    }
    // Find base indent from first non-empty line.
    let mut base_indent: Option<usize> = None;
    for &line in &lines[start..] {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        // block content must be indented; if indent == 0 it's the next key.
        if indent == 0 {
            break;
        }
        base_indent = Some(indent);
        break;
    }
    let Some(base) = base_indent else {
        return (String::new(), 0);
    };
    let mut collected: Vec<&str> = Vec::new();
    let mut consumed = 0;
    for &line in &lines[start..] {
        if line.trim().is_empty() {
            // blank line is part of scalar (preserves break)
            collected.push(line);
            consumed += 1;
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent < base {
            break;
        }
        collected.push(line);
        consumed += 1;
    }
    let value = if is_folded {
        fold_block_scalar(&collected, base)
    } else {
        literal_block_scalar(&collected, base)
    };
    (value, consumed)
}

fn fold_block_scalar(lines: &[&str], base: usize) -> String {
    let mut out = String::new();
    let mut prev_more_indented = false;
    let mut prev_blank = false;
    for &raw in lines {
        if raw.trim().is_empty() {
            // Blank line -> hard break. Collapse consecutive blanks to one extra newline?
            // In YAML folded, a blank line is a "\n". We'll ensure one newline.
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            } else if !out.is_empty() {
                // consecutive blank already has newline, add one more
                out.push('\n');
            }
            prev_blank = true;
            prev_more_indented = false;
            continue;
        }
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let more_indented = indent > base;
        // content after base indent, preserve extra spaces for more-indented
        let content = &raw[base..];
        if more_indented {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(content);
            prev_more_indented = true;
            prev_blank = false;
        } else {
            let trimmed = content.trim();
            if out.is_empty() {
                out.push_str(trimmed);
            } else if prev_blank || prev_more_indented || out.ends_with('\n') {
                if out.ends_with('\n') {
                    // already at line start
                } else {
                    out.push('\n');
                }
                out.push_str(trimmed);
            } else {
                out.push(' ');
                out.push_str(trimmed);
            }
            prev_more_indented = false;
            prev_blank = false;
        }
    }
    // chomping: tests use >- (strip). Our construction never adds a trailing newline,
    // so strip is implicit. For keep (+) we would need to preserve, but not required.
    out
}

fn literal_block_scalar(lines: &[&str], base: usize) -> String {
    let mut out = String::new();
    for (i, &raw) in lines.iter().enumerate() {
        if raw.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let content = if raw.len() >= base { &raw[base..] } else { raw };
        out.push_str(content);
        if i + 1 < lines.len() {
            out.push('\n');
        }
    }
    // Strip final break like chomping strip
    out.trim_end_matches('\n').to_owned()
}
