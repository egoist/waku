#!/usr/bin/env bun

/**
 * Seeds oversized synthetic sessions into the dev database so transcript
 * performance can be measured against something far larger than real history.
 *
 * The rows written here are exactly what the Rust side writes: a narrow
 * `sessions` row, the rest of `AgentSession` as JSON in `session_details`
 * (minus `messages`, which the app keeps in its own table), and one `messages`
 * row per message. Nothing about the shapes is invented — see `src/model.rs`
 * and `session_data` in `src/persistence.rs`.
 *
 * Every id it writes starts with `9e5e0000-`, so a reseed can delete precisely
 * what a previous run added and never touch real sessions.
 *
 *   bun ./scripts/seed-mock-sessions.ts            # (re)seed all profiles
 *   bun ./scripts/seed-mock-sessions.ts --scale 2  # twice the volume
 *   bun ./scripts/seed-mock-sessions.ts --only tool-storm
 *   bun ./scripts/seed-mock-sessions.ts --clean    # remove them again
 *
 * The app reads sessions once at startup, so a running debug build only shows
 * these after its next relaunch.
 */

import { Database } from "bun:sqlite";
import { readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");

/** Shared prefix of every id this script owns; `--clean` matches on it. */
const MOCK_ID_PREFIX = "9e5e0000-";

type Options = {
  databasePath: string;
  projectRef: string | undefined;
  scale: number;
  seed: number;
  only: string[];
  clean: boolean;
};

function parseOptions(argv: string[]): Options {
  const options: Options = {
    databasePath: join(root, "temp/app.db"),
    projectRef: undefined,
    scale: 1,
    seed: 20260806,
    only: [],
    clean: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined) throw new Error(`${flag} needs a value`);
      index += 1;
      return next;
    };
    switch (flag) {
      case "--db":
        options.databasePath = resolve(value());
        break;
      case "--project":
        options.projectRef = value();
        break;
      case "--scale":
        options.scale = Number(value());
        break;
      case "--seed":
        options.seed = Number(value());
        break;
      case "--only":
        options.only = value().split(",").map((key) => key.trim());
        break;
      case "--clean":
        options.clean = true;
        break;
      default:
        throw new Error(`unknown flag: ${flag}`);
    }
  }
  if (!Number.isFinite(options.scale) || options.scale <= 0) {
    throw new Error("--scale must be a positive number");
  }
  return options;
}

// ---------------------------------------------------------------------------
// Deterministic randomness
// ---------------------------------------------------------------------------

/** mulberry32 — same seed, same database, so a run is reproducible. */
function makeRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Random = () => number;

const pick = <T>(random: Random, values: readonly T[]): T =>
  values[Math.floor(random() * values.length)]!;

const int = (random: Random, min: number, max: number): number =>
  min + Math.floor(random() * (max - min + 1));

const chance = (random: Random, probability: number): boolean =>
  random() < probability;

function mockUuid(random: Random): string {
  const hex = (count: number) =>
    Array.from({ length: count }, () => "0123456789abcdef"[Math.floor(random() * 16)]).join("");
  // Valid v4 layout so `Uuid::parse_str` accepts it, with a fixed first group
  // marking it as ours.
  return `${MOCK_ID_PREFIX}${hex(4)}-4${hex(3)}-8${hex(3)}-${hex(12)}`;
}

// ---------------------------------------------------------------------------
// Content banks
//
// Realistic-looking material matters here: the cost being measured is markdown
// parsing, syntax highlighting, text shaping and wrapping, so the mock content
// has to have the same shape as what an agent actually produces.
// ---------------------------------------------------------------------------

const PROMPTS = [
  "the transcript stutters when I scroll fast through a long session, can you profile it",
  "make the sidebar remember which project was selected across restarts",
  "why does hydrating a session take so long on startup?",
  "add a keyboard shortcut to collapse every turn in the transcript",
  "the composer loses focus after a turn finishes, fix that",
  "reasoning blocks should keep their scroll position when a turn is expanded",
  "audit the render path for anything touching the filesystem",
  "tool output over a few thousand lines should virtualize instead of laying out everything",
  "the diff view wraps long lines in a way that breaks alignment, take a look",
  "can we cache the markdown parse between frames instead of reparsing",
  "session switching drops a frame or two, find out where the time goes",
  "make the activity disclosure animate open without janking the list",
  "the checkpoint row runs git on the ui thread somewhere, track it down",
  "add a setting for the transcript font size and make it live-update",
  "long code blocks inside assistant messages should get a copy button",
  "measure how much time we spend in text shaping for a 2000 message session",
  "the turn fold summary should say how many tools ran, not just the duration",
  "scrolling to the bottom on a new message should be instant, not animated",
  "check whether we re-measure every row when only the last one changed",
  "images in tool output are decoded on the ui thread, move that off",
];

const HEADINGS = [
  "What I found",
  "Root cause",
  "The fix",
  "Measurements",
  "Why it was slow",
  "Trade-offs",
  "What changed",
  "Follow-ups",
  "How it works now",
  "Verification",
];

const SENTENCES = [
  "The row builder was rebuilding whole-session state for every visible item, so the cost scaled with history rather than with what is on screen.",
  "Each frame walked the full transcript to compute anchors, which is fine at fifty messages and ruinous at five thousand.",
  "I hoisted that into a cache refreshed once per frame and keyed it on the fingerprint the fold already computes.",
  "The measurement pass shapes text twice: once for the height query and once for paint, and neither result was retained.",
  "Nothing here needs the filesystem, but the checkpoint affordance was reaching for `git` before the background prefetch had landed.",
  "Because the list is virtualized, only the visible window matters — the trap is any per-frame work that iterates the whole collection anyway.",
  "Deserializing the detail blob dominates session switching once a transcript passes a few megabytes.",
  "I kept the allocation out of the hot path by reusing the buffer between rows rather than collecting into a fresh vector.",
  "The generation counter guards against a superseded background pass overwriting newer state when two switches land close together.",
  "Wrapping is the expensive part: every soft break costs a shaping run, and code blocks defeat the fast path entirely.",
  "Syntax highlighting now runs once per code block and the result is stored beside the parsed block rather than recomputed on scroll.",
  "The regression only shows on a high-refresh display, where the frame budget is under seven milliseconds.",
  "I left the synchronous path in place for one-shot actions, where freshness is worth more than latency.",
  "Interleaving the blocks at message boundaries keeps ordering stable even when a provider emits events out of order.",
  "That turned a linear scan per row into a single hash map lookup, which is what made the difference under a long transcript.",
];

const INLINE_SNIPPETS = [
  "`transcript_rows_fingerprint`",
  "`folded_transcript_row_kinds`",
  "`ListState::splice`",
  "`cx.background_executor().spawn`",
  "`AgentSession::hydrate`",
  "`session_details.data`",
  "`render` → `measure` → `paint`",
  "`Window::request_animation_frame`",
];

const FILES = [
  "src/app/transcript.rs",
  "src/app/transcript_view.rs",
  "src/app/components.rs",
  "src/app/streaming.rs",
  "src/app/sessions.rs",
  "src/md/render.rs",
  "src/md/parser.rs",
  "src/md/highlight.rs",
  "src/persistence.rs",
  "src/model.rs",
  "src/driver/codex.rs",
  "src/ui/scrollbar.rs",
];

const CODE_SAMPLES: { language: string; lines: string[] }[] = [
  {
    language: "rust",
    lines: [
      "pub(super) fn visible_rows(&self, window: &Window) -> Range<usize> {",
      "    let viewport = window.viewport_size().height;",
      "    let first = self.offsets.partition_point(|offset| *offset < self.scroll_top);",
      "    let last = self.offsets.partition_point(|offset| *offset < self.scroll_top + viewport);",
      "    first..last.min(self.rows.len())",
      "}",
      "",
      "impl TranscriptView {",
      "    fn remeasure(&mut self, kind: TranscriptRowKind, cx: &mut Context<Self>) {",
      "        let Some(index) = self.rows.iter().position(|row| *row == kind) else {",
      "            return;",
      "        };",
      "        self.list.splice(index..index + 1, 1);",
      "        cx.notify();",
      "    }",
      "}",
    ],
  },
  {
    language: "rust",
    lines: [
      "let cached = self.row_cache.borrow();",
      "if cached.fingerprint == fingerprint {",
      "    return cached.rows.clone();",
      "}",
      "drop(cached);",
      "",
      "let rows = folded_transcript_row_kinds(session, &self.expanded_turns);",
      "*self.row_cache.borrow_mut() = RowCache {",
      "    fingerprint,",
      "    rows: rows.clone(),",
      "};",
      "rows",
    ],
  },
  {
    language: "typescript",
    lines: [
      "const rows = useMemo(() => foldTranscript(session, expanded), [session, expanded]);",
      "",
      "useLayoutEffect(() => {",
      "  if (!listRef.current) return;",
      "  listRef.current.scrollToIndex(rows.length - 1, { align: 'end' });",
      "}, [rows.length]);",
      "",
      "export function foldTranscript(session: Session, expanded: Set<string>) {",
      "  const anchors = session.blocks.map((block) => block.afterMessage);",
      "  return interleave(session.messages.length, anchors).filter((row) =>",
      "    expanded.has(row.turnId) || !row.hidden,",
      "  );",
      "}",
    ],
  },
  {
    language: "bash",
    lines: [
      "cargo build --profile dev 2>&1 | tail -20",
      "hyperfine --warmup 3 'target/debug/waku --bench transcript'",
      "rg -n 'fn render' src/app | wc -l",
      "sqlite3 temp/app.db 'EXPLAIN QUERY PLAN SELECT * FROM sessions ORDER BY updated_at'",
    ],
  },
  {
    language: "json",
    lines: [
      "{",
      '  "session": "long-history",',
      '  "messages": 4021,',
      '  "blocks": 6144,',
      '  "frame_ms": { "p50": 4.1, "p95": 11.7, "max": 38.2 },',
      '  "hydrate_ms": 182.4',
      "}",
    ],
  },
  {
    language: "diff",
    lines: [
      "-    let rows = folded_transcript_row_kinds(session, &self.expanded_turns);",
      "+    let fingerprint = transcript_rows_fingerprint(session, &self.expanded_turns);",
      "+    let rows = self.cached_rows(session, fingerprint);",
      "     for row in &rows {",
      "-        self.measure(row, window, cx);",
      "+        self.measure_if_dirty(row, window, cx);",
      "     }",
    ],
  },
];

const COMMANDS = [
  "cargo build",
  "cargo test transcript",
  "cargo clippy --all-targets",
  "rg -n 'transcript_rows_fingerprint' src",
  "git diff --stat",
  "sqlite3 temp/app.db 'select count(*) from messages'",
  "wc -l src/app/*.rs",
  "bun ./scripts/dev.ts",
];

const TOOL_TITLES = ["read", "edit", "write", "glob", "grep", "todowrite", "webfetch"];

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

function paragraph(random: Random, sentences = int(random, 2, 5)): string {
  const parts: string[] = [];
  for (let index = 0; index < sentences; index += 1) {
    let sentence = pick(random, SENTENCES);
    if (chance(random, 0.3)) {
      sentence = sentence.replace(/\.$/, ` — see ${pick(random, INLINE_SNIPPETS)}.`);
    }
    parts.push(sentence);
  }
  return parts.join(" ");
}

function codeFence(random: Random, minLines: number): string {
  const sample = pick(random, CODE_SAMPLES);
  const lines: string[] = [];
  let repeat = 0;
  while (lines.length < minLines) {
    for (const line of sample.lines) {
      // Vary the repeats so highlighting and wrapping never see identical input.
      lines.push(repeat === 0 ? line : line.replace(/\b(\d+)\b/g, (digits) => String(Number(digits) + repeat)));
    }
    repeat += 1;
    if (repeat > 40) break;
  }
  return ["```" + sample.language, ...lines, "```"].join("\n");
}

function table(random: Random, rows = int(random, 3, 9)): string {
  const lines = [
    "| Path | Rows | p50 (ms) | p95 (ms) |",
    "| --- | ---: | ---: | ---: |",
  ];
  for (let index = 0; index < rows; index += 1) {
    lines.push(
      `| \`${pick(random, FILES)}\` | ${int(random, 40, 9000)} | ${(random() * 6).toFixed(2)} | ${(random() * 24).toFixed(2)} |`,
    );
  }
  return lines.join("\n");
}

function bulletList(random: Random, items = int(random, 3, 7)): string {
  return Array.from({ length: items }, () => {
    const nested = chance(random, 0.35)
      ? `\n  - ${pick(random, SENTENCES).slice(0, 90)}`
      : "";
    return `- ${paragraph(random, 1)}${nested}`;
  }).join("\n");
}

function taskList(random: Random, items = int(random, 3, 6)): string {
  return Array.from(
    { length: items },
    () => `- [${chance(random, 0.6) ? "x" : " "}] ${paragraph(random, 1).slice(0, 110)}`,
  ).join("\n");
}

function numberedList(random: Random, items = int(random, 3, 6)): string {
  return Array.from(
    { length: items },
    (_, index) => `${index + 1}. **${pick(random, HEADINGS)}** — ${paragraph(random, 1)}`,
  ).join("\n");
}

/** A markdown answer of roughly `targetBytes`, mixing every construct the
 * renderer supports so parsing, highlighting, tables and wrapping all get hit. */
function markdownAnswer(random: Random, targetBytes: number): string {
  const parts: string[] = [`### ${pick(random, HEADINGS)}`, paragraph(random)];
  let size = parts.join("\n\n").length;
  while (size < targetBytes) {
    const roll = random();
    const part =
      roll < 0.3
        ? paragraph(random)
        : roll < 0.5
          ? codeFence(random, int(random, 6, 40))
          : roll < 0.63
            ? bulletList(random)
            : roll < 0.72
              ? numberedList(random)
              : roll < 0.79
                ? taskList(random)
                : roll < 0.86
                  ? table(random)
                  : roll < 0.93
                    ? `> ${paragraph(random, 2)}`
                    : `#### ${pick(random, HEADINGS)}`;
    parts.push(part);
    size += part.length + 2;
  }
  if (chance(random, 0.5)) {
    parts.push(
      `See [\`${pick(random, FILES)}\`](${pick(random, FILES)}) and ~~the old path~~ for the rest.`,
    );
  }
  return parts.join("\n\n");
}

function buildLog(random: Random, lines: number): string {
  const out: string[] = [];
  for (let index = 0; index < lines; index += 1) {
    const roll = random();
    if (roll < 0.08) {
      out.push(
        `warning: unused variable: \`${pick(random, ["index", "cx", "window", "session"])}\``,
      );
      out.push(`  --> ${pick(random, FILES)}:${int(random, 20, 1800)}:${int(random, 4, 60)}`);
      out.push("   |");
      out.push(`${String(int(random, 20, 1800)).padStart(4)} |     let ${pick(random, ["rows", "anchors", "cache"])} = session.transcript_blocks.len();`);
      out.push("   |         ^^^^ help: if this is intentional, prefix it with an underscore");
    } else if (roll < 0.2) {
      out.push(
        `   Compiling ${pick(random, ["gpui", "waku", "rusqlite", "pulldown-cmark", "smol"])} v${int(random, 0, 3)}.${int(random, 0, 40)}.${int(random, 0, 9)}`,
      );
    } else {
      out.push(
        `test ${pick(random, ["app::tests", "md::parser::tests", "persistence::tests"])}::${pick(random, ["folds_settled_turns", "keeps_scroll_anchor", "hydrates_one_session", "writes_only_dirty_rows"])}_${index} ... ok`,
      );
    }
  }
  out.push("");
  out.push(
    `test result: ok. ${lines} passed; 0 failed; 0 ignored; finished in ${(random() * 30).toFixed(2)}s`,
  );
  return out.join("\n");
}

function unifiedDiff(random: Random, hunks: number): string {
  const path = pick(random, FILES);
  const out = [`diff --git a/${path} b/${path}`, `--- a/${path}`, `+++ b/${path}`];
  for (let index = 0; index < hunks; index += 1) {
    const start = int(random, 20, 1600);
    out.push(`@@ -${start},${int(random, 6, 18)} +${start},${int(random, 6, 20)} @@`);
    const sample = pick(random, CODE_SAMPLES);
    for (const line of sample.lines) {
      const roll = random();
      out.push(roll < 0.25 ? `-${line}` : roll < 0.6 ? `+${line}` : ` ${line}`);
    }
  }
  return out.join("\n");
}

function searchResults(random: Random, hits: number): string {
  const out: string[] = [];
  for (let index = 0; index < hits; index += 1) {
    const sample = pick(random, CODE_SAMPLES);
    out.push(
      `${pick(random, FILES)}:${int(random, 10, 1900)}:${pick(random, sample.lines).trim()}`,
    );
  }
  return out.join("\n");
}

function reasoningText(random: Random, targetBytes: number): string {
  const parts: string[] = [];
  let size = 0;
  while (size < targetBytes) {
    const part = paragraph(random, int(random, 2, 6));
    parts.push(part);
    size += part.length + 2;
  }
  return parts.join("\n\n");
}

// ---------------------------------------------------------------------------
// Session assembly
// ---------------------------------------------------------------------------

type Activity = {
  id: string;
  source_id: string | null;
  kind: "reasoning" | "command" | "fileChange" | "search" | "plan" | "tool";
  title: string;
  detail: string | null;
  arguments?: string;
  output?: string;
  image_urls?: string[];
  failed: boolean;
  complete: boolean;
};

type Block = {
  after_message: number;
  turn_id: string;
  content:
    | { kind: "reasoning"; data: { content: string; started_at_ms: number; finished_at_ms: number } }
    | { kind: "activities"; data: Activity[] };
};

type MessageRow = {
  id: string;
  turn_id: string;
  position: number;
  role: "user" | "assistant";
  content: string;
  created_at: number;
};

type Profile = {
  key: string;
  title: string;
  provider: string;
  model: string;
  /** Turns before `--scale`. */
  turns: number;
  answerBytes: [number, number];
  answersPerTurn: [number, number];
  activityBlocksPerTurn: [number, number];
  activitiesPerBlock: [number, number];
  outputLines: [number, number];
  reasoningBytes: [number, number];
  reasoningBlocksPerTurn: [number, number];
  imageChance: number;
};

const PROFILES: Profile[] = [
  {
    // Row count: the fingerprint and fold walk every message and block each
    // frame, so this one is about sheer length rather than heavy rows.
    key: "long-history",
    title: "Perf · long history (thousands of turns)",
    provider: "codex",
    model: "gpt-5.2-codex",
    turns: 2000,
    answerBytes: [300, 1400],
    answersPerTurn: [1, 2],
    activityBlocksPerTurn: [1, 3],
    activitiesPerBlock: [1, 2],
    outputLines: [6, 30],
    reasoningBytes: [200, 900],
    reasoningBlocksPerTurn: [0, 1],
    imageChance: 0,
  },
  {
    // Markdown: tables, fenced code, nested lists — parsing, highlighting and
    // wrapping, which is where per-row measurement time goes.
    key: "markdown-heavy",
    title: "Perf · markdown and code heavy answers",
    provider: "claude",
    model: "claude-opus-5",
    turns: 260,
    answerBytes: [6000, 22000],
    answersPerTurn: [1, 3],
    activityBlocksPerTurn: [1, 2],
    activitiesPerBlock: [1, 2],
    outputLines: [10, 60],
    reasoningBytes: [400, 1600],
    reasoningBlocksPerTurn: [0, 2],
    imageChance: 0,
  },
  {
    // Tool activity: huge outputs and many disclosure sections behind a fold.
    // Expanding one of these turns is the worst case for the transcript.
    key: "tool-storm",
    title: "Perf · tool activity storm (expand a turn)",
    provider: "grok",
    model: "grok-4.5",
    turns: 90,
    answerBytes: [400, 2500],
    answersPerTurn: [1, 2],
    activityBlocksPerTurn: [15, 40],
    activitiesPerBlock: [1, 2],
    outputLines: [30, 150],
    reasoningBytes: [800, 3500],
    reasoningBlocksPerTurn: [1, 3],
    imageChance: 0.12,
  },
  {
    // Everything at once, and the biggest detail blob — the hydrate-cost test.
    key: "kitchen-sink",
    title: "Perf · everything at maximum",
    provider: "openCode",
    model: "gpt-5.2",
    turns: 700,
    answerBytes: [800, 9000],
    answersPerTurn: [1, 3],
    activityBlocksPerTurn: [2, 8],
    activitiesPerBlock: [1, 3],
    outputLines: [15, 70],
    reasoningBytes: [400, 3000],
    reasoningBlocksPerTurn: [0, 3],
    imageChance: 0.05,
  },
];

function blobReferences(databasePath: string): string[] {
  const blobsRoot = join(resolve(databasePath, ".."), "blobs");
  try {
    if (!statSync(blobsRoot).isDirectory()) return [];
  } catch {
    return [];
  }
  const references: string[] = [];
  for (const shard of readdirSync(blobsRoot)) {
    let files: string[];
    try {
      files = readdirSync(join(blobsRoot, shard));
    } catch {
      continue;
    }
    for (const file of files) {
      if (/\.(png|jpe?g|gif|webp)$/i.test(file)) references.push(`waku-blob:${file}`);
    }
  }
  return references;
}

function makeActivity(
  random: Random,
  profile: Profile,
  images: string[],
): Activity {
  const roll = random();
  const id = mockUuid(random);
  const sourceId = `call_${Math.floor(random() * 1e12).toString(36)}`;
  const lines = int(random, profile.outputLines[0], profile.outputLines[1]);

  if (roll < 0.3) {
    const command = pick(random, COMMANDS);
    return {
      id,
      source_id: sourceId,
      kind: "command",
      title: "bash",
      detail: JSON.stringify({ command }),
      arguments: JSON.stringify({ command, cwd: root }, null, 2),
      output: buildLog(random, lines),
      failed: chance(random, 0.06),
      complete: true,
    };
  }
  if (roll < 0.5) {
    const path = pick(random, FILES);
    return {
      id,
      source_id: sourceId,
      kind: "fileChange",
      title: chance(random, 0.5) ? "edit" : "write",
      detail: `+${int(random, 3, 220)} −${int(random, 0, 90)}`,
      arguments: JSON.stringify({ filePath: join(root, path) }, null, 2),
      output: unifiedDiff(random, int(random, 1, 6)),
      failed: false,
      complete: true,
    };
  }
  if (roll < 0.68) {
    const query = pick(random, ["transcript row", "fingerprint", "ListState", "hydrate", "measure"]);
    return {
      id,
      source_id: sourceId,
      kind: "search",
      title: chance(random, 0.5) ? "grep" : "websearch",
      detail: JSON.stringify({ query }),
      arguments: JSON.stringify({ query, path: "src", limit: 50 }, null, 2),
      output: searchResults(random, Math.max(4, Math.floor(lines / 3))),
      failed: false,
      complete: true,
    };
  }
  if (roll < 0.74) {
    return {
      id,
      source_id: sourceId,
      kind: "plan",
      title: "todowrite",
      detail: null,
      arguments: JSON.stringify(
        {
          todos: Array.from({ length: int(random, 3, 8) }, (_, index) => ({
            content: paragraph(random, 1).slice(0, 80),
            status: index === 0 ? "in_progress" : "pending",
          })),
        },
        null,
        2,
      ),
      output: taskList(random, int(random, 3, 8)),
      failed: false,
      complete: true,
    };
  }

  const withImage = images.length > 0 && chance(random, profile.imageChance);
  return {
    id,
    source_id: sourceId,
    kind: "tool",
    title: pick(random, TOOL_TITLES),
    detail: JSON.stringify({ filePath: join(root, pick(random, FILES)) }),
    arguments: JSON.stringify(
      { title: `Inspecting ${pick(random, FILES)}`, limit: int(random, 50, 400) },
      null,
      2,
    ),
    output: withImage ? undefined : searchResults(random, Math.max(3, Math.floor(lines / 4))),
    image_urls: withImage
      ? Array.from({ length: int(random, 1, 3) }, () => pick(random, images))
      : undefined,
    failed: chance(random, 0.04),
    complete: true,
  };
}

type BuiltSession = {
  id: string;
  title: string;
  provider: string;
  model: string;
  createdAt: number;
  updatedAt: number;
  lastReplyAt: number;
  messages: MessageRow[];
  blockCount: number;
  detail: string;
};

function buildSession(
  profile: Profile,
  options: Options,
  projectId: string,
  images: string[],
  index: number,
): BuiltSession {
  const random = makeRandom(options.seed + index * 7919);
  const sessionId = mockUuid(random);
  const turnCount = Math.max(1, Math.round(profile.turns * options.scale));

  const messages: MessageRow[] = [];
  const blocks: Block[] = [];
  const turns: unknown[] = [];

  // Walk backwards from now so the newest turn is the last one, and the whole
  // session lands in a plausible recent window.
  const now = Math.floor(Date.now() / 1000);
  let clock = now - turnCount * 45 - index * 3600;
  const createdAt = clock;

  for (let turnIndex = 0; turnIndex < turnCount; turnIndex += 1) {
    const turnId = mockUuid(random);
    const startedAt = clock;

    messages.push({
      id: mockUuid(random),
      turn_id: turnId,
      position: messages.length,
      role: "user",
      content: `${pick(random, PROMPTS)}${chance(random, 0.25) ? `\n\n\`\`\`\n${pick(random, COMMANDS)}\n\`\`\`` : ""}`,
      created_at: clock,
    });
    clock += int(random, 2, 12);

    // Work for this turn: reasoning and activity blocks anchored after the
    // prompt, exactly where the streaming path would have put them.
    const anchor = messages.length;
    const reasoningBlocks = int(random, profile.reasoningBlocksPerTurn[0], profile.reasoningBlocksPerTurn[1]);
    const activityBlocks = int(random, profile.activityBlocksPerTurn[0], profile.activityBlocksPerTurn[1]);
    const work: Block[] = [];
    for (let blockIndex = 0; blockIndex < reasoningBlocks; blockIndex += 1) {
      const startedMs = clock * 1000;
      work.push({
        after_message: anchor,
        turn_id: turnId,
        content: {
          kind: "reasoning",
          data: {
            content: reasoningText(random, int(random, profile.reasoningBytes[0], profile.reasoningBytes[1])),
            started_at_ms: startedMs,
            finished_at_ms: startedMs + int(random, 400, 9000),
          },
        },
      });
    }
    for (let blockIndex = 0; blockIndex < activityBlocks; blockIndex += 1) {
      const count = int(random, profile.activitiesPerBlock[0], profile.activitiesPerBlock[1]);
      work.push({
        after_message: anchor,
        turn_id: turnId,
        content: {
          kind: "activities",
          data: Array.from({ length: count }, () => makeActivity(random, profile, images)),
        },
      });
    }
    // Interleave so reasoning and tool calls alternate the way they arrive.
    for (let position = work.length - 1; position > 0; position -= 1) {
      const swap = Math.floor(random() * (position + 1));
      [work[position], work[swap]] = [work[swap]!, work[position]!];
    }
    blocks.push(...work);
    clock += int(random, 5, 90);

    const answers = int(random, profile.answersPerTurn[0], profile.answersPerTurn[1]);
    for (let answerIndex = 0; answerIndex < answers; answerIndex += 1) {
      messages.push({
        id: mockUuid(random),
        turn_id: turnId,
        position: messages.length,
        role: "assistant",
        content: markdownAnswer(random, int(random, profile.answerBytes[0], profile.answerBytes[1])),
        created_at: clock,
      });
      clock += int(random, 1, 8);
    }

    const status = chance(random, 0.04) ? "interrupted" : chance(random, 0.03) ? "failed" : "completed";
    turns.push({
      id: turnId,
      turn_count: turnIndex + 1,
      status,
      provider_turn_started: true,
      started_at: startedAt,
      completed_at: clock,
      checkpoint: chance(random, 0.7)
        ? {
            turn_count: turnIndex + 1,
            // No such ref exists, so the rewind affordance stays hidden —
            // `prefetch_checkpoint_refs` only offers it for refs git resolves.
            git_ref: `refs/waku/session-${sessionId}-turn-${turnIndex + 1}`,
            status: "ready",
            files: Array.from({ length: int(random, 0, 5) }, () => ({
              path: pick(random, FILES),
              additions: int(random, 1, 300),
              deletions: int(random, 0, 120),
            })),
            created_at: clock,
          }
        : null,
    });
    clock += int(random, 20, 900);
  }

  // Clamped so a long walk cannot land in the future, staggered so the seeded
  // sessions sort deterministically at the top of the list instead of tying.
  const updatedAt = Math.min(clock, now - index * 60);
  const detail = JSON.stringify({
    id: sessionId,
    title: profile.title,
    project_id: projectId,
    provider: profile.provider,
    model: profile.model,
    runtime_mode: "fullAccess",
    status: "idle",
    created_at: createdAt,
    updated_at: updatedAt,
    last_reply_at: updatedAt,
    // Left unset on purpose: these transcripts have no provider session to
    // resume, and a bogus cursor would send the driver looking for one.
    provider_cursor: null,
    turns,
    transcript_blocks: blocks,
  });

  return {
    id: sessionId,
    title: profile.title,
    provider: profile.provider,
    model: profile.model,
    createdAt,
    updatedAt,
    lastReplyAt: updatedAt,
    messages,
    blockCount: blocks.length,
    detail,
  };
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

function deleteMockSessions(database: Database): number {
  const like = `${MOCK_ID_PREFIX}%`;
  const before = database
    .query<{ count: number }, [string]>("SELECT count(*) AS count FROM sessions WHERE id LIKE ?")
    .get(like)!.count;
  database.run("DELETE FROM messages WHERE session_id LIKE ?", [like]);
  database.run("DELETE FROM session_details WHERE session_id LIKE ?", [like]);
  database.run("DELETE FROM sessions WHERE id LIKE ?", [like]);
  return before;
}

function resolveProject(database: Database, ref: string | undefined): { id: string; name: string } {
  const projects = database
    .query<{ id: string; name: string }, []>("SELECT id, name FROM projects ORDER BY position")
    .all();
  if (projects.length === 0) {
    throw new Error("no projects in the database — add one in the app first");
  }
  if (!ref) return projects[0]!;
  const match = projects.find((project) => project.id === ref || project.name === ref);
  if (!match) {
    throw new Error(
      `no project matching "${ref}" (have: ${projects.map((project) => project.name).join(", ")})`,
    );
  }
  return match;
}

const formatBytes = (bytes: number): string =>
  bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${(bytes / 1024).toFixed(0)} KB`;

function main(): void {
  const options = parseOptions(process.argv.slice(2));
  const database = new Database(options.databasePath, { create: false, readwrite: true });
  // The debug app usually holds this database open; wait rather than fail if it
  // happens to be mid-write.
  database.run("PRAGMA busy_timeout = 10000");

  if (options.clean) {
    const removed = database.transaction(() => deleteMockSessions(database))();
    console.log(`[seed] removed ${removed} mock session(s) from ${options.databasePath}`);
    database.close();
    return;
  }

  const project = resolveProject(database, options.projectRef);
  const images = blobReferences(options.databasePath);
  const profiles = options.only.length
    ? PROFILES.filter((profile) => options.only.includes(profile.key))
    : PROFILES;
  if (profiles.length === 0) {
    throw new Error(`--only matched nothing (have: ${PROFILES.map((p) => p.key).join(", ")})`);
  }

  console.log(
    `[seed] project ${project.name} · scale ${options.scale} · ${images.length} blob(s) available`,
  );

  const messageBytes = (session: BuiltSession) =>
    session.messages.reduce((sum, message) => sum + Buffer.byteLength(message.content), 0);

  const built = profiles.map((profile, index) => {
    const session = buildSession(profile, options, project.id, images, index);
    console.log(
      `[seed] ${profile.key.padEnd(15)} ${String(session.messages.length).padStart(6)} messages · ` +
        `${String(session.blockCount).padStart(6)} blocks · ` +
        `${formatBytes(messageBytes(session)).padStart(8)} text · ` +
        `${formatBytes(Buffer.byteLength(session.detail)).padStart(8)} detail`,
    );
    return session;
  });

  const insertSession = database.prepare(
    `INSERT INTO sessions(id, project_id, title, provider, model, status, created_at, updated_at, last_reply_at)
     VALUES (?, ?, ?, ?, ?, 'idle', ?, ?, ?)`,
  );
  const insertDetail = database.prepare(
    "INSERT INTO session_details(session_id, data) VALUES (?, ?)",
  );
  const insertMessage = database.prepare(
    `INSERT INTO messages(id, session_id, turn_id, position, role, content, created_at, streaming)
     VALUES (?, ?, ?, ?, ?, ?, ?, 0)`,
  );

  const write = database.transaction(() => {
    const removed = deleteMockSessions(database);
    for (const session of built) {
      insertSession.run(
        session.id,
        project.id,
        session.title,
        session.provider,
        session.model,
        session.createdAt,
        session.updatedAt,
        session.lastReplyAt,
      );
      insertDetail.run(session.id, session.detail);
      for (const message of session.messages) {
        insertMessage.run(
          message.id,
          session.id,
          message.turn_id,
          message.position,
          message.role,
          message.content,
          message.created_at,
        );
      }
    }
    return removed;
  });

  const removed = write();
  database.run("PRAGMA wal_checkpoint(PASSIVE)");
  const totalMessages = built.reduce((sum, session) => sum + session.messages.length, 0);
  const totalBytes = built.reduce(
    (sum, session) => sum + Buffer.byteLength(session.detail) + messageBytes(session),
    0,
  );
  database.close();

  console.log(
    `[seed] replaced ${removed} → wrote ${built.length} sessions · ${totalMessages} messages · ${formatBytes(totalBytes)} of transcript`,
  );
  console.log("[seed] relaunch the debug app to see them — sessions are read once at startup");
}

main();
