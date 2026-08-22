/// Parse the leading markdown frontmatter block used by commands and skills.
///
/// This intentionally recognizes only the keys Waku cares about and simple
/// scalar strings, including YAML folded and literal block scalars. Invalid or
/// unsupported lines are skipped so a hand-written prompt still stays listed.
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

    let mut lines = block.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        if let Some(value) = frontmatter_value(value, &mut lines) {
            visit(key, value);
        }
    }

    body.trim_start_matches(['-']).trim_start()
}

fn frontmatter_value<'a, I>(value: &str, lines: &mut std::iter::Peekable<I>) -> Option<String>
where
    I: Iterator<Item = &'a str>,
{
    if let Some(style) = block_scalar_style(value) {
        return block_scalar_value(style, lines);
    }

    let value = value.trim_matches('"').trim_matches('\'').trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn block_scalar_style(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let style = chars.next()?;
    if style != '>' && style != '|' {
        return None;
    }
    chars
        .all(|character| character == '-' || character == '+' || character.is_ascii_digit())
        .then_some(style)
}

fn block_scalar_value<'a, I>(style: char, lines: &mut std::iter::Peekable<I>) -> Option<String>
where
    I: Iterator<Item = &'a str>,
{
    let mut raw_lines = Vec::new();
    while let Some(line) = lines.peek().copied() {
        if !line.trim().is_empty() && leading_whitespace(line) == 0 {
            break;
        }
        raw_lines.push(line);
        lines.next();
    }

    let indent = raw_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading_whitespace(line))
        .min()
        .unwrap_or(0);
    let lines = raw_lines
        .into_iter()
        .map(|line| line.get(indent..).unwrap_or(line))
        .collect::<Vec<_>>();

    let value = if style == '>' {
        fold_block_lines(&lines)
    } else {
        lines.join("\n")
    };
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn leading_whitespace(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn fold_block_lines(lines: &[&str]) -> String {
    let mut folded = String::new();
    let mut blank_lines = 0;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            blank_lines += 1;
            continue;
        }
        if !folded.is_empty() {
            if blank_lines == 0 {
                folded.push(' ');
            } else {
                for _ in 0..blank_lines {
                    folded.push('\n');
                }
            }
        }
        folded.push_str(line);
        blank_lines = 0;
    }
    folded
}
