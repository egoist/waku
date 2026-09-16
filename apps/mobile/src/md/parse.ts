/**
 * Streaming markdown parser over an append-only source.
 *
 * Port of the desktop's `src/md/parser.rs` incremental scheme onto mdast
 * (micromark + GFM): the settled prefix of top-level blocks is never
 * reparsed; each append reparses only the tail from the last stable
 * boundary, and `displayTail` re-derives the final block with hanging inline
 * markers mended (see `./mend`) so a closing `**` never reflows text that is
 * already on screen.
 *
 * Only top-level block offsets are tracked (in UTF-16 code units, shifted to
 * the full document). Inner mdast positions are slice-relative and must not
 * be read — the renderer keys everything on visible text, not source offsets.
 */

import type { RootContent } from 'mdast';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { gfmFromMarkdown } from 'mdast-util-gfm';
import { gfm } from 'micromark-extension-gfm';

import { closeHanging } from './mend';

export interface MarkdownBlock {
  node: RootContent;
  /** Source range in the full document, in UTF-16 code units. */
  start: number;
  end: number;
  /** Source slice: block identity for render caching. */
  source: string;
}

export function parseBlocks(text: string, offset = 0): MarkdownBlock[] {
  const root = fromMarkdown(text, {
    extensions: [gfm()],
    mdastExtensions: [gfmFromMarkdown()],
  });
  return root.children.map((node) => {
    const start = node.position?.start.offset ?? 0;
    const end = node.position?.end.offset ?? text.length;
    return {
      node,
      start: offset + start,
      end: offset + end,
      source: text.slice(start, end),
    };
  });
}

export class IncrementalMarkdown {
  private textValue = '';
  private blocksValue: MarkdownBlock[] = [];
  /** Blocks before this index are settled: no append can change them. */
  private stablePrefix = 0;
  /** A link or footnote definition anywhere forces full reparses. */
  private fullReparseOnly = false;
  private displayCache: { text: string; tail: MarkdownBlock[] | null } | null = null;

  get text(): string {
    return this.textValue;
  }

  get blocks(): readonly MarkdownBlock[] {
    return this.blocksValue;
  }

  /** Index of the first block an append could still change. */
  get settled(): number {
    return this.stablePrefix;
  }

  /** Point the parser at `text`. Appends reparse incrementally; any other
   * change falls back to a full reparse. */
  setText(text: string) {
    if (text === this.textValue) return;
    if (
      this.textValue &&
      !this.fullReparseOnly &&
      text.startsWith(this.textValue)
    ) {
      this.append(text.slice(this.textValue.length));
    } else {
      this.reset(text);
    }
  }

  /** Discard all state and parse `text` from scratch. */
  reset(text: string) {
    this.textValue = text;
    this.blocksValue = parseBlocks(text);
    this.fullReparseOnly = hasLinkDefinition(text);
    this.stablePrefix = this.settledPrefix();
    this.displayCache = null;
  }

  /** Append `delta`, reparsing only from the last stable block boundary. */
  append(delta: string) {
    if (!delta) return;
    if (this.fullReparseOnly) {
      this.reset(this.textValue + delta);
      return;
    }

    const boundary =
      this.blocksValue[this.stablePrefix]?.start ?? this.textValue.length;
    this.textValue += delta;
    if (hasLinkDefinition(delta)) {
      this.reset(this.textValue);
      return;
    }

    const tail = parseBlocks(this.textValue.slice(boundary), boundary);
    this.blocksValue = [...this.blocksValue.slice(0, this.stablePrefix), ...tail];
    this.stablePrefix = this.settledPrefix();
    this.displayCache = null;
  }

  /**
   * Replacement blocks for the final block while streaming, with its hanging
   * inline markers closed so styling does not flip as the closer arrives.
   * `null` means the canonical blocks already render correctly.
   */
  displayTail(): MarkdownBlock[] | null {
    if (this.displayCache?.text === this.textValue) {
      return this.displayCache.tail;
    }
    const tail = this.deriveDisplayTail();
    this.displayCache = { text: this.textValue, tail };
    return tail;
  }

  private deriveDisplayTail(): MarkdownBlock[] | null {
    const last = this.blocksValue.at(-1);
    if (!last) return null;
    // A code block's content is literal: mending would corrupt it, and a
    // half-typed fence must not be reinterpreted.
    if (last.node.type === 'code') return null;
    const mended = closeHanging(this.textValue.slice(last.start));
    if (mended === null) return null;
    return parseBlocks(mended, last.start).map((block) => ({
      ...block,
      end: Math.min(block.end, this.textValue.length),
    }));
  }

  /**
   * Appending mostly only extends the final block, but constructs like a GFM
   * table absorbing its next row, a setext underline claiming the paragraph
   * above, or two lists merging across a blank line reach one block further
   * back — so the last two top-level blocks stay unsettled, mirroring the
   * desktop parser's two-group rule.
   */
  private settledPrefix(): number {
    return Math.max(0, this.blocksValue.length - 2);
  }
}

/** Cheap scan for a link reference definition (`[label]: destination`) or a
 * footnote definition, which resolve references anywhere in the document and
 * so break locality. */
export function hasLinkDefinition(text: string): boolean {
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trimStart();
    if (!line.startsWith('[')) continue;
    const end = line.indexOf(']:');
    if (end > 1) return true;
  }
  return false;
}
