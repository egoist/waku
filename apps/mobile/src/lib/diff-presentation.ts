export type DiffLineKind = 'hunk' | 'add' | 'remove' | 'context' | 'gap';

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
  oldLine: number | null;
  newLine: number | null;
  hiddenLines?: number;
}

const MAX_DIFF_LINES = 400;
const DEFAULT_CONTEXT_LINES = 3;

interface ParseUnifiedDiffOptions {
  /** Collapse unchanged runs while retaining this many rows beside changes. */
  contextLines?: number;
  /** Positioned hunk headers are redundant when numbered rows are shown. */
  hidePositionedHunks?: boolean;
}

/**
 * Parses the normalized unified diff carried on ActivityFileChange.diff.
 * File headers are dropped (the surrounding UI already names the file) and
 * hunk headers may be a bare `@@` when the provider never reported positions.
 */
export function parseUnifiedDiff(
  diff: string,
  options: ParseUnifiedDiffOptions = {},
): { lines: DiffLine[]; truncated: boolean } {
  const parsed: DiffLine[] = [];
  const compact = options.contextLines !== undefined;
  let oldLine: number | null = null;
  let newLine: number | null = null;
  let previousOldNext = 1;
  let previousNewNext = 1;

  for (const raw of diff.split('\n')) {
    if (raw.startsWith('diff --git')) {
      oldLine = null;
      newLine = null;
      previousOldNext = 1;
      previousNewNext = 1;
      continue;
    }
    if (
      raw.startsWith('index ') ||
      raw.startsWith('--- ') ||
      raw.startsWith('+++ ') ||
      raw === '---' ||
      raw === '+++' ||
      raw.startsWith('\\ No newline')
    ) {
      continue;
    }
    if (raw.startsWith('@@')) {
      const starts = parseHunkStarts(raw);
      if (starts) {
        if (compact) {
          const hidden = Math.max(
            starts.oldLine - previousOldNext,
            starts.newLine - previousNewNext,
          );
          if (hidden > 0) appendGap(parsed, hidden);
        }
        oldLine = starts.oldLine;
        newLine = starts.newLine;
        if (!options.hidePositionedHunks) {
          parsed.push({
            kind: 'hunk',
            text: raw.trim(),
            oldLine: null,
            newLine: null,
          });
        }
      } else {
        oldLine = null;
        newLine = null;
        parsed.push({
          kind: 'hunk',
          text: raw.trim(),
          oldLine: null,
          newLine: null,
        });
      }
      continue;
    }
    if (raw.startsWith('+')) {
      parsed.push({ kind: 'add', text: raw.slice(1), oldLine: null, newLine });
      if (newLine !== null) {
        newLine += 1;
        previousNewNext = newLine;
      }
      continue;
    }
    if (raw.startsWith('-')) {
      parsed.push({ kind: 'remove', text: raw.slice(1), oldLine, newLine: null });
      if (oldLine !== null) {
        oldLine += 1;
        previousOldNext = oldLine;
      }
      continue;
    }
    parsed.push({
      kind: 'context',
      text: raw.startsWith(' ') ? raw.slice(1) : raw,
      oldLine,
      newLine,
    });
    if (oldLine !== null) {
      oldLine += 1;
      previousOldNext = oldLine;
    }
    if (newLine !== null) {
      newLine += 1;
      previousNewNext = newLine;
    }
  }

  while (
    parsed.length &&
    parsed.at(-1)!.text === '' &&
    parsed.at(-1)!.kind === 'context'
  ) {
    parsed.pop();
  }

  const presented = compact
    ? collapseContext(parsed, Math.max(0, options.contextLines ?? DEFAULT_CONTEXT_LINES))
    : parsed;
  return {
    lines: presented.slice(0, MAX_DIFF_LINES),
    truncated: presented.length > MAX_DIFF_LINES,
  };
}

export function diffStats(lines: DiffLine[]): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const line of lines) {
    if (line.kind === 'add') additions += 1;
    else if (line.kind === 'remove') deletions += 1;
  }
  return { additions, deletions };
}

function parseHunkStarts(raw: string): { oldLine: number; newLine: number } | null {
  const match = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
  if (!match?.[1] || !match[2]) return null;
  return {
    oldLine: Number.parseInt(match[1], 10),
    newLine: Number.parseInt(match[2], 10),
  };
}

function collapseContext(lines: DiffLine[], contextLines: number): DiffLine[] {
  const changeAfter = new Array<boolean>(lines.length + 1).fill(false);
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    changeAfter[index] = changeAfter[index + 1] || isChange(lines[index]!);
  }

  const collapsed: DiffLine[] = [];
  let sawChange = false;
  let index = 0;
  while (index < lines.length) {
    const line = lines[index]!;
    if (line.kind !== 'context') {
      sawChange ||= isChange(line);
      collapsed.push(line);
      index += 1;
      continue;
    }

    const start = index;
    while (index < lines.length && lines[index]!.kind === 'context') index += 1;
    const run = lines.slice(start, index);
    const hasLaterChange = changeAfter[index]!;
    if (!sawChange && hasLaterChange) {
      appendCollapsedLeading(collapsed, run, contextLines);
    } else if (sawChange && !hasLaterChange) {
      appendCollapsedTrailing(collapsed, run, contextLines);
    } else if (sawChange && hasLaterChange) {
      appendCollapsedBetween(collapsed, run, contextLines);
    } else {
      collapsed.push(...run);
    }
  }
  return collapsed;
}

function appendCollapsedLeading(
  output: DiffLine[],
  lines: DiffLine[],
  contextLines: number,
) {
  const kept = Math.min(contextLines, lines.length);
  const hidden = lines.length - kept;
  if (hidden <= 1) {
    output.push(...lines);
    return;
  }
  appendGap(output, hidden);
  output.push(...lines.slice(lines.length - kept));
}

function appendCollapsedTrailing(
  output: DiffLine[],
  lines: DiffLine[],
  contextLines: number,
) {
  const kept = Math.min(contextLines, lines.length);
  const hidden = lines.length - kept;
  if (hidden <= 1) {
    output.push(...lines);
    return;
  }
  output.push(...lines.slice(0, kept));
  appendGap(output, hidden);
}

function appendCollapsedBetween(
  output: DiffLine[],
  lines: DiffLine[],
  contextLines: number,
) {
  const keptStart = Math.min(contextLines, lines.length);
  const keptEnd = Math.min(contextLines, lines.length - keptStart);
  const hidden = lines.length - keptStart - keptEnd;
  if (hidden <= 1) {
    output.push(...lines);
    return;
  }
  output.push(...lines.slice(0, keptStart));
  appendGap(output, hidden);
  output.push(...lines.slice(lines.length - keptEnd));
}

function appendGap(lines: DiffLine[], hiddenLines: number) {
  if (hiddenLines <= 0) return;
  const previous = lines.at(-1);
  if (previous?.kind === 'gap') {
    previous.hiddenLines = (previous.hiddenLines ?? 0) + hiddenLines;
    return;
  }
  lines.push({
    kind: 'gap',
    text: '',
    oldLine: null,
    newLine: null,
    hiddenLines,
  });
}

function isChange(line: DiffLine): boolean {
  return line.kind === 'add' || line.kind === 'remove';
}
