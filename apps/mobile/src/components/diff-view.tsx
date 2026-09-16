import { useMemo } from 'react';
import { ScrollView, StyleSheet, Text, View } from 'react-native';

import { MonoFont, Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { parseUnifiedDiff, type DiffLine } from '@/lib/diff-presentation';

/** Colored unified-diff body rendered into an inset code well. */
export function DiffView({
  diff,
  mode = 'well',
}: {
  diff: string;
  mode?: 'well' | 'review';
}) {
  const theme = useTheme();
  const { lines, truncated } = useMemo(
    () =>
      parseUnifiedDiff(
        diff,
        mode === 'review'
          ? { contextLines: 3, hidePositionedHunks: true }
          : undefined,
      ),
    [diff, mode],
  );
  if (!lines.length) return null;
  if (mode === 'review') {
    return (
      <View style={styles.reviewLines}>
        {lines.map((line, index) => (
          <ReviewDiffRow key={index} line={line} />
        ))}
        {truncated ? (
          <Text
            style={[
              styles.reviewTruncated,
              { color: theme.textTertiary, borderColor: theme.border },
            ]}
          >
            … diff truncated
          </Text>
        ) : null}
      </View>
    );
  }
  return (
    <View style={[styles.well, { backgroundColor: theme.inset, borderColor: theme.border }]}>
      <ScrollView horizontal showsHorizontalScrollIndicator={false}>
        <View style={styles.lines}>
          {lines.map((line, index) => (
            <DiffRow key={index} line={line} />
          ))}
          {truncated && (
            <Text style={[styles.truncated, { color: theme.textTertiary }]}>
              … diff truncated
            </Text>
          )}
        </View>
      </ScrollView>
    </View>
  );
}

function ReviewDiffRow({ line }: { line: DiffLine }) {
  const theme = useTheme();
  if (line.kind === 'gap' || line.kind === 'hunk') {
    const label =
      line.kind === 'gap'
        ? `${line.hiddenLines ?? 0} unmodified ${line.hiddenLines === 1 ? 'line' : 'lines'}`
        : line.text;
    return (
      <View style={[styles.reviewRow, { backgroundColor: theme.overlay }]}>
        <View
          style={[
            styles.reviewGutter,
            { borderColor: theme.border, backgroundColor: theme.overlay },
          ]}
        />
        <Text
          selectable
          style={[styles.reviewMeta, { color: theme.textTertiary }]}
        >
          {label}
        </Text>
      </View>
    );
  }

  const changed = line.kind === 'add' || line.kind === 'remove';
  const semanticColor = line.kind === 'add' ? theme.success : theme.danger;
  const background =
    line.kind === 'add'
      ? theme.successSoft
      : line.kind === 'remove'
        ? theme.dangerSoft
        : 'transparent';
  const shownLine = line.newLine ?? line.oldLine;
  return (
    <View
      style={[
        styles.reviewRow,
        {
          backgroundColor: background,
          borderLeftColor: changed ? semanticColor : 'transparent',
        },
      ]}
    >
      <View
        style={[
          styles.reviewGutter,
          {
            backgroundColor: changed ? background : 'transparent',
            borderColor: theme.border,
          },
        ]}
      >
        <Text
          style={[
            styles.reviewLineNumber,
            { color: changed ? semanticColor : theme.textTertiary },
          ]}
        >
          {shownLine ?? ''}
        </Text>
      </View>
      <Text
        selectable
        style={[styles.reviewCode, { color: theme.textSecondary }]}
      >
        {line.text || ' '}
      </Text>
    </View>
  );
}

function DiffRow({ line }: { line: DiffLine }) {
  const theme = useTheme();
  if (line.kind === 'hunk') {
    return (
      <Text style={[styles.line, styles.hunk, { color: theme.textTertiary }]}>{line.text}</Text>
    );
  }
  const background = line.kind === 'add'
    ? theme.successSoft
    : line.kind === 'remove'
      ? theme.dangerSoft
      : 'transparent';
  const color = line.kind === 'add'
    ? theme.success
    : line.kind === 'remove'
      ? theme.danger
      : theme.textSecondary;
  const marker = line.kind === 'add' ? '+' : line.kind === 'remove' ? '−' : ' ';
  return (
    <View style={[styles.row, { backgroundColor: background }]}>
      <Text style={[styles.marker, { color }]}>{marker}</Text>
      <Text style={[styles.line, { color }]}>{line.text || ' '}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  well: {
    borderRadius: Radius.small,
    borderWidth: StyleSheet.hairlineWidth,
    overflow: 'hidden',
    paddingVertical: 6,
  },
  lines: { minWidth: '100%', paddingHorizontal: 8 },
  row: { borderRadius: 3, flexDirection: 'row', paddingRight: 10 },
  marker: { fontFamily: MonoFont, fontSize: 11, lineHeight: 16.5, width: 12 },
  line: { fontFamily: MonoFont, fontSize: 11, lineHeight: 16.5 },
  hunk: { fontFamily: MonoFont, fontSize: 10.5, lineHeight: 18, marginVertical: 2 },
  truncated: { fontSize: 10.5, marginTop: 4 },
  reviewLines: { minWidth: '100%', overflow: 'hidden' },
  reviewRow: {
    alignItems: 'stretch',
    borderLeftWidth: 2,
    flexDirection: 'row',
    minHeight: 20,
  },
  reviewGutter: {
    alignItems: 'flex-end',
    borderRightWidth: StyleSheet.hairlineWidth,
    justifyContent: 'flex-start',
    paddingRight: 8,
    paddingTop: 2,
    width: 42,
  },
  reviewLineNumber: {
    fontFamily: MonoFont,
    fontSize: 10.5,
    lineHeight: 17,
  },
  reviewCode: {
    flex: 1,
    fontFamily: MonoFont,
    fontSize: 11,
    lineHeight: 17,
    paddingHorizontal: 10,
    paddingVertical: 2,
  },
  reviewMeta: {
    flex: 1,
    fontFamily: MonoFont,
    fontSize: 10.5,
    lineHeight: 17,
    paddingHorizontal: 10,
    paddingVertical: 2,
  },
  reviewTruncated: {
    borderTopWidth: StyleSheet.hairlineWidth,
    fontSize: 10.5,
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
});
