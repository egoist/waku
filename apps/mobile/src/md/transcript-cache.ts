/**
 * Per-session markdown parse cache.
 *
 * One incremental parser per *streaming* message (the tail re-parses
 * O(delta + tail) per commit and block node identities in the settled prefix
 * are stable), plus a memo of settled parses so rebuilding the row array on
 * every commit re-parses nothing. The live→settled handoff adopts the live
 * parser's tree, so row identities never change at the flip.
 */

import { IncrementalMarkdown, parseBlocks, type MarkdownBlock } from './parse';

interface CompletedParse {
  source: string;
  blocks: readonly MarkdownBlock[];
}

export class TranscriptMarkdownCache {
  private parsers = new Map<string, IncrementalMarkdown>();
  private completed = new Map<string, CompletedParse>();

  blocksFor(messageId: string, text: string, streaming: boolean): readonly MarkdownBlock[] {
    if (streaming) {
      let parser = this.parsers.get(messageId);
      if (!parser) {
        parser = new IncrementalMarkdown();
        this.parsers.set(messageId, parser);
      }
      parser.setText(text);
      return parser.blocks;
    }
    const handoff = this.parsers.get(messageId);
    this.parsers.delete(messageId);
    const hit = this.completed.get(messageId);
    if (hit && hit.source === text) return hit.blocks;
    const blocks = handoff && handoff.text === text ? handoff.blocks : parseBlocks(text);
    this.completed.set(messageId, { source: text, blocks });
    return blocks;
  }

  /** The live parser for a streaming message — the live row derives its
   * mended display tail from it. */
  parserFor(messageId: string): IncrementalMarkdown | undefined {
    return this.parsers.get(messageId);
  }

  /** Drop memos for messages that no longer exist. The count guard keeps the
   * common append-only rebuild from copying the map every commit. */
  prune(liveMessageIds: ReadonlySet<string>) {
    if (this.completed.size <= liveMessageIds.size) return;
    for (const key of this.completed.keys()) {
      if (!liveMessageIds.has(key)) this.completed.delete(key);
    }
  }
}
