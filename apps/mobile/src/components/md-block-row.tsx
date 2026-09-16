import { memo, useEffect, useMemo, useReducer, useRef, type ReactNode } from 'react';
import { Animated, StyleSheet } from 'react-native';

import { MonoFont } from '@/constants/theme';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import type { MarkdownBlock } from '@/md/parse';
import {
  openLinkExternally,
  renderBlock,
  type MarkdownStyles,
  type RenderContext,
} from '@/md/render';
import type { TranscriptMarkdownCache } from '@/md/transcript-cache';
import { RowVeil } from '@/md/veil';

/**
 * Markdown metrics — the desktop's render.rs values: body 14/22, headings
 * 19/27 · 16/24 · 15/22 · 14/22, code 12.5/18. Block-level vertical rhythm
 * lives on the transcript rows (`topGap`), so blocks carry no margins of their
 * own.
 */
export function useMarkdownStyles(): MarkdownStyles {
  const theme = useTheme();
  return useMemo<MarkdownStyles>(() => {
    const heading = (size: number, lineHeight: number) => ({
      color: theme.text,
      fontSize: size,
      fontWeight: '600' as const,
      lineHeight,
    });
    return {
      body: { color: theme.text, fontSize: 14, lineHeight: 22 },
      paragraph: {},
      heading: [
        heading(19, 27),
        heading(16, 24),
        heading(15, 22),
        heading(14, 22),
        heading(14, 22),
        heading(14, 22),
      ],
      strong: { fontWeight: '600' },
      em: { fontStyle: 'italic' },
      strikethrough: { color: theme.textSecondary, textDecorationLine: 'line-through' },
      // Monochrome links: primary ink with a muted hairline underline, never
      // accent (desktop render.rs).
      link: {
        color: theme.text,
        textDecorationColor: theme.textTertiary,
        textDecorationLine: 'underline',
      },
      codespan: {
        backgroundColor: theme.codeWash,
        color: theme.codeText,
        fontFamily: MonoFont,
        fontSize: 12.5,
      },
      codeBlock: {
        backgroundColor: theme.inset,
        borderColor: theme.border,
        borderRadius: 10,
        borderWidth: StyleSheet.hairlineWidth,
        overflow: 'hidden',
      },
      codeHeader: {
        borderBottomColor: theme.border,
        borderBottomWidth: StyleSheet.hairlineWidth,
        paddingHorizontal: 12,
        paddingVertical: 5,
      },
      codeHeaderText: { color: theme.textTertiary, fontSize: 11 },
      codeContent: { paddingHorizontal: 12, paddingVertical: 10 },
      codeLine: {
        color: theme.text,
        fontFamily: MonoFont,
        fontSize: 12.5,
        lineHeight: 18,
      },
      blockquote: {
        backgroundColor: theme.accentSoft,
        borderBottomRightRadius: 6,
        borderLeftColor: theme.accent,
        borderLeftWidth: 2,
        borderTopRightRadius: 6,
        gap: 8,
        paddingLeft: 12,
        paddingRight: 10,
        paddingVertical: 6,
      },
      list: { gap: 4 },
      listItem: { flexDirection: 'row', gap: 8 },
      listMarker: {
        color: theme.accent,
        fontSize: 14,
        fontVariant: ['tabular-nums'],
        lineHeight: 22,
        minWidth: 18,
        textAlign: 'right',
      },
      listContent: { flex: 1, gap: 4 },
      // Frameless tables: hairline row rules only — no outer border, no
      // header fill, no cell borders (desktop parity).
      table: {},
      tableRow: {
        borderBottomColor: theme.separator,
        borderBottomWidth: StyleSheet.hairlineWidth,
        flexDirection: 'row',
      },
      tableHeadRow: {},
      tableCell: { minWidth: 48, padding: 12 },
      tableCellText: { color: theme.text, fontSize: 14, lineHeight: 22 },
      tableHeadText: { color: theme.text, fontSize: 14, fontWeight: '600', lineHeight: 22 },
      hr: { backgroundColor: theme.border, height: 1 },
      image: {
        backgroundColor: theme.inset,
        borderRadius: 10,
        height: 200,
        width: '100%',
      },
    };
  }, [theme]);
}

/** One RowVeil per live row, dropped on the live→settled flip. */
export class VeilRegistry {
  private veils = new Map<string, RowVeil>();

  get(rowKey: string, seeded: boolean): RowVeil {
    let veil = this.veils.get(rowKey);
    if (!veil) {
      veil = new RowVeil(seeded);
      this.veils.set(rowKey, veil);
    }
    return veil;
  }

  drop(rowKey: string) {
    this.veils.delete(rowKey);
  }
}

function staticContext(styles: MarkdownStyles): RenderContext {
  return {
    styles,
    veil: null,
    now: 0,
    ordinal: { value: 0 },
    hadSpans: false,
    onOpenLink: openLinkExternally,
  };
}

/**
 * Blocks born settled while their message streams (a fast commit can close a
 * paragraph and mint two list items at once) fade in as whole rows — without
 * this, everything that lands outside the live tail pops at full alpha and
 * the reveal reads as jank. Compositor-driven; layout is final at mount.
 */
export function MdRevealOnMount({
  enabled,
  children,
}: {
  enabled: boolean;
  children: ReactNode;
}) {
  const reducedMotion = useReducedMotion();
  const fade = enabled && !reducedMotion;
  const opacity = useRef(new Animated.Value(fade ? 0 : 1)).current;
  useEffect(() => {
    if (!fade) return;
    Animated.timing(opacity, {
      duration: 240,
      toValue: 1,
      useNativeDriver: true,
    }).start();
  }, [fade, opacity]);
  if (!fade) return <>{children}</>;
  return <Animated.View style={{ opacity }}>{children}</Animated.View>;
}

/** A settled markdown block. Re-renders only when its source changes — the
 * incremental parser keeps node identity stable across commits, so streamed
 * tokens never touch this. */
export const MdBlockSettled = memo(
  function MdBlockSettled({
    node,
    styles,
  }: {
    node: MarkdownBlock['node'];
    source: string;
    styles: MarkdownStyles;
  }) {
    return <>{renderBlock(node, staticContext(styles), 'b')}</>;
  },
  (previous, next) => previous.source === next.source && previous.styles === next.styles,
);

/**
 * The live tail block of the streaming message: renders the mended display
 * tail, dissolving appended text in via the veil. The veil gate applies — only
 * paragraphs and headings veil; structural blocks (code, lists, tables)
 * appear settled-style.
 */
export function MdBlockLive({
  rowKey,
  messageId,
  node,
  md,
  veils,
  seeded,
  styles,
}: {
  rowKey: string;
  messageId: string;
  node: MarkdownBlock['node'];
  source: string;
  md: TranscriptMarkdownCache;
  veils: VeilRegistry;
  seeded: boolean;
  styles: MarkdownStyles;
}) {
  const [, bump] = useReducer((count: number) => count + 1, 0);
  const reducedMotion = useReducedMotion();
  useEffect(() => () => veils.drop(rowKey), [rowKey, veils]);

  const tail = md.parserFor(messageId)?.displayTail() ?? null;
  const blocks = tail ?? [{ node }];
  // Every live block type veils — a list item or code line popping at full
  // alpha next to fading prose reads as jank, not structure.
  const veil = reducedMotion ? null : veils.get(rowKey, seeded);
  const ctx: RenderContext = {
    styles,
    veil,
    now: Date.now(),
    ordinal: { value: 0 },
    hadSpans: false,
    onOpenLink: openLinkExternally,
  };
  veil?.beginFrame();
  const children = blocks.map((block, index) => renderBlock(block.node, ctx, index));
  veil?.finishFrame();
  const fading = veil?.isFading() ?? false;
  useEffect(() => {
    if (!fading) return;
    const timer = setTimeout(bump, 33);
    return () => clearTimeout(timer);
  });
  return <>{children}</>;
}
