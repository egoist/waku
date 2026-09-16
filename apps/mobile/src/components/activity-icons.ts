import type { ActivityKind } from '@waku/client';
import type { SymbolViewProps } from 'expo-symbols';

/** One glyph per activity kind, shared by the transcript card and its
 * detail sheet so the two read as the same thing. */
export const ACTIVITY_ICONS: Record<ActivityKind, SymbolViewProps['name']> = {
  reasoning: { ios: 'sparkles', android: 'auto_awesome', web: 'auto_awesome' },
  command: { ios: 'terminal', android: 'terminal', web: 'terminal' },
  fileChange: { ios: 'pencil.line', android: 'edit', web: 'edit' },
  fileRead: { ios: 'doc.text', android: 'description', web: 'description' },
  fileSearch: { ios: 'doc.text.magnifyingglass', android: 'find_in_page', web: 'find_in_page' },
  fileList: { ios: 'folder', android: 'folder', web: 'folder' },
  search: { ios: 'globe', android: 'travel_explore', web: 'travel_explore' },
  plan: { ios: 'checklist', android: 'checklist', web: 'checklist' },
  tool: { ios: 'wrench.and.screwdriver', android: 'build', web: 'build' },
};
