import { describe, expect, test } from "bun:test";
import type { AgentSession } from "@waku/client";

import {
  latestReviewTurnSource,
  parseNumstat,
  splitReviewPatch,
} from "./task-surfaces";

describe("task surface presentation", () => {
  test("splits a multi-file review patch and keeps deleted file names", () => {
    const files = splitReviewPatch(`diff --git a/src/a.ts b/src/a.ts
--- a/src/a.ts
+++ b/src/a.ts
@@ -1 +1 @@
-old
+new
diff --git a/src/gone.ts b/src/gone.ts
--- a/src/gone.ts
+++ /dev/null
@@ -1 +0,0 @@
-gone`);

    expect(files.map((file) => file.path)).toEqual(["src/a.ts", "src/gone.ts"]);
    expect(files[0]!.patch).toContain("+new");
  });

  test("counts text numstat while treating binary markers as zero", () => {
    expect(parseNumstat("3\t1\tsrc/a.ts\n-\t-\timage.png")).toEqual({
      files: 2,
      additions: 3,
      deletions: 1,
    });
  });

  test("selects the latest ready checkpoint for last-turn review", () => {
    const session = {
      id: "session",
      turns: [
        { id: "one", turn_count: 1, checkpoint: { status: "ready" } },
        { id: "two", turn_count: 2, checkpoint: { status: "failed" } },
      ],
    } as AgentSession;
    expect(latestReviewTurnSource(session)).toEqual({
      lastTurn: { session_id: "session", turn_id: "one", turn_count: 1 },
    });
  });
});
