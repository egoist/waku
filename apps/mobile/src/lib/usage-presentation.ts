import type {
  ModelSlice,
  ProviderSlice,
  UsageHistory,
  UsageProvider,
  UsageWindow,
} from '@waku/client';

/** Windows the daemon can scan, in picker order. Keys match web's usage page
 * so a window selected on one client reads the same on the other. */
export const USAGE_WINDOWS: Array<{ window: UsageWindow; label: string }> = [
  { window: { trailingDays: 7 }, label: 'Last 7 days' },
  { window: { trailingDays: 30 }, label: 'Last 30 days' },
  { window: { trailingDays: 90 }, label: 'Last 90 days' },
  { window: 'thisMonth', label: 'This month' },
  { window: 'lastMonth', label: 'Last month' },
];

export type UsageMetric = 'cost' | 'tokens';

/** Stable identity for a window, so the picker can match the one the daemon
 * echoed back in `history.window`. */
export function usageWindowKey(window: UsageWindow): string {
  if (typeof window === 'string') return window;
  return 'trailingDays' in window ? `days:${window.trailingDays}` : `months:${window.months}`;
}

export function usageWindowLabel(window: UsageWindow): string {
  const key = usageWindowKey(window);
  return USAGE_WINDOWS.find((option) => usageWindowKey(option.window) === key)?.label ?? 'Custom';
}

/** Spend needs cents while it is small and none once it is not; web's rule,
 * kept so a phone and a browser show the same number for the same window. */
export function formatMoney(value: number): string {
  return `$${groupDigits(value.toFixed(value < 10 ? 2 : 0))}`;
}

/** Compact token counts: the transcript cares about magnitude, not units. */
export function formatTokens(value: number): string {
  if (value >= 999_500) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return `${Math.round(value)}`;
}

/** Shares arrive as 0..1 fractions from the daemon. */
export function formatPercent(share: number): string {
  return `${Math.round(share * 100)}%`;
}

export function formatCount(value: number): string {
  return groupDigits(`${Math.round(value)}`);
}

/** Day keys are UTC on the daemon host; format them in UTC so the range the
 * user reads matches the days the scan actually covered. */
export function formatUsageRange(
  sinceDay: string,
  untilDay: string,
  locale?: string,
): string {
  const sameYear = sinceDay.slice(0, 4) === untilDay.slice(0, 4);
  const format = new Intl.DateTimeFormat(locale, {
    day: 'numeric',
    month: 'short',
    timeZone: 'UTC',
    year: sameYear ? undefined : 'numeric',
  });
  return `${format.format(parseDay(sinceDay))} – ${format.format(parseDay(untilDay))}`;
}

/** Countdown to a plan window's reset. Past-due resets read as "soon" rather
 * than a negative duration the provider cannot actually promise. */
export function planResetLabel(
  resetsAt: number,
  nowSeconds: number = Math.floor(Date.now() / 1_000),
  locale?: string,
): string {
  const seconds = resetsAt - nowSeconds;
  if (seconds <= 0) return 'Resets soon';
  const minutes = Math.ceil(seconds / 60);
  if (minutes < 60) return `Resets in ${minutes}m`;
  if (minutes < 24 * 60) {
    const hours = Math.floor(minutes / 60);
    const remainder = minutes % 60;
    return remainder ? `Resets in ${hours}h ${remainder}m` : `Resets in ${hours}h`;
  }
  const date = new Intl.DateTimeFormat(locale, {
    hour: 'numeric',
    minute: '2-digit',
    weekday: 'short',
  }).format(new Date(resetsAt * 1_000));
  return `Resets ${date}`;
}

/** Usage history reports spend per CLI, not per Waku provider id, and Claude's
 * CLI is branded "Claude Code" everywhere else in the product. */
export function usageProviderLabel(provider: UsageProvider): string {
  return provider === 'claude' ? 'Claude Code' : 'Codex';
}

export function sortedProviders(history: UsageHistory, metric: UsageMetric): ProviderSlice[] {
  return [...history.providers].sort((left, right) => (
    metric === 'cost' ? right.costUsd - left.costUsd : right.totalTokens - left.totalTokens
  ));
}

export function topModels(history: UsageHistory, limit = 6): ModelSlice[] {
  return [...history.models].sort((left, right) => right.costUsd - left.costUsd).slice(0, limit);
}

/** Footer line: what the scan actually read, so an empty window is
 * distinguishable from a scan that never ran. */
export function scanSummary(history: UsageHistory): string {
  const seconds = history.scanDuration.secs + history.scanDuration.nanos / 1_000_000_000;
  return [
    `${formatCount(history.scannedFiles)} files scanned`,
    `${formatCount(history.records)} records`,
    `${seconds.toFixed(1)}s`,
  ].join(' · ');
}

function parseDay(day: string): Date {
  return new Date(`${day}T00:00:00Z`);
}

function groupDigits(value: string): string {
  const [whole, fraction] = value.split('.');
  const grouped = (whole ?? '').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return fraction ? `${grouped}.${fraction}` : grouped;
}
