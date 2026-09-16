import type { AgentSession, ReviewDiffSource } from "@waku/client";

export interface ReviewPatchFile {
  key: string;
  path: string;
  patch: string;
}

export function latestReviewTurnSource(
  session: AgentSession,
): ReviewDiffSource | null {
  for (let index = session.turns.length - 1; index >= 0; index -= 1) {
    const turn = session.turns[index]!;
    if (turn.turn_count > 0 && turn.checkpoint?.status === "ready") {
      return {
        lastTurn: {
          session_id: session.id,
          turn_id: turn.id,
          turn_count: turn.turn_count,
        },
      };
    }
  }
  return null;
}

export function reviewDiffSourceLabel(source: ReviewDiffSource): string {
  if (typeof source === "object") return `Turn ${source.lastTurn.turn_count}`;
  return {
    uncommitted: "Uncommitted",
    unstaged: "Unstaged",
    staged: "Staged",
    committed: "Committed",
    branch: "Branch",
  }[source];
}

export function parseNumstat(numstat: string): {
  files: number;
  additions: number;
  deletions: number;
} {
  let files = 0;
  let additions = 0;
  let deletions = 0;
  for (const line of numstat.trim().split("\n")) {
    if (!line) continue;
    const [added, removed] = line.split("\t");
    files += 1;
    additions += Number.parseInt(added ?? "", 10) || 0;
    deletions += Number.parseInt(removed ?? "", 10) || 0;
  }
  return { files, additions, deletions };
}

/** Split one Git patch into file-sized cards. The daemon already normalizes
 * the diff; this only recovers a stable display label for the compact mobile
 * review surface. */
export function splitReviewPatch(patch: string): ReviewPatchFile[] {
  const chunks = patch
    .split(/(?=^diff --git )/m)
    .map((chunk) => chunk.trim())
    .filter(Boolean);

  return chunks.map((chunk, index) => {
    const lines = chunk.split("\n");
    const path =
      diffMarkerPath(lines, "+++") ??
      diffMarkerPath(lines, "---") ??
      diffHeaderPath(lines) ??
      `Changed file ${index + 1}`;
    return { key: `${index}:${path}`, path, patch: chunk };
  });
}

function diffMarkerPath(lines: string[], marker: "+++" | "---"): string | null {
  const prefix = `${marker} `;
  const raw = lines
    .find((line) => line.startsWith(prefix))
    ?.slice(prefix.length)
    .trim();
  if (!raw || raw === "/dev/null") return null;
  return cleanDiffPath(raw);
}

function diffHeaderPath(lines: string[]): string | null {
  const header = lines.find((line) => line.startsWith("diff --git "));
  if (!header) return null;
  const match = header.match(/ b\/(.+)$/);
  return match?.[1] ? cleanDiffPath(match[1]) : null;
}

function cleanDiffPath(path: string): string {
  let value = path;
  if (value.startsWith('"') && value.endsWith('"')) {
    try {
      value = JSON.parse(value) as string;
    } catch {
      value = value.slice(1, -1);
    }
  }
  return value.replace(/^[ab]\//, "");
}
