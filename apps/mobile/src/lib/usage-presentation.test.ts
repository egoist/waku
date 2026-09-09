import { describe, expect, test } from 'bun:test';
import type { CostQuality, TokenTotals, UsageHistory, UsageWindow } from '@waku/client';

import {
  USAGE_WINDOWS,
  formatCount,
  formatMoney,
  formatPercent,
  formatTokens,
  formatUsageRange,
  planResetLabel,
  scanSummary,
  sortedProviders,
  topModels,
  usageProviderLabel,
  usageWindowKey,
  usageWindowLabel,
} from './usage-presentation';

const totals: TokenTotals = {
  uncachedInput: 10,
  cachedInput: 20,
  cacheCreation: 30,
  output: 40,
  reasoning: 50,
};

const quality: CostQuality = {
  providerReportedShare: 0.5,
  modelPricedShare: 0.4,
  unpricedShare: 0.1,
  cacheSavingsUsd: 3,
};

function history(overrides: Partial<UsageHistory> = {}): UsageHistory {
  return {
    window: { trailingDays: 30 },
    sinceDay: '2026-02-01',
    untilDay: '2026-03-03',
    totals,
    totalTokens: 1_234_000,
    costUsd: 12.5,
    records: 4_200,
    sessions: 9,
    providers: [
      { provider: 'codex', costUsd: 10, totalTokens: 900, costShare: 0.8, tokenShare: 0.6 },
      { provider: 'claude', costUsd: 2.5, totalTokens: 1_500, costShare: 0.2, tokenShare: 0.4 },
    ],
    models: [
      { provider: 'codex', model: 'gpt-5', costUsd: 9, totalTokens: 800, costShare: 0.72 },
      { provider: 'claude', model: 'opus-4', costUsd: 2.5, totalTokens: 700, costShare: 0.2 },
      { provider: 'claude', model: 'sonnet-4', costUsd: 1, totalTokens: 100, costShare: 0.08 },
    ],
    daily: [],
    months: [],
    projects: [],
    quality,
    pricing: 'fresh',
    scannedFiles: 1_500,
    skippedFiles: 2,
    errors: [],
    scanDuration: { secs: 1, nanos: 250_000_000 },
    ...overrides,
  };
}

describe('usage windows', () => {
  test('keys every shape the daemon accepts', () => {
    expect(usageWindowKey({ trailingDays: 7 })).toBe('days:7');
    expect(usageWindowKey({ months: 12 })).toBe('months:12');
    expect(usageWindowKey('thisMonth')).toBe('thisMonth');
  });

  test('labels a known window and falls back for a custom one', () => {
    expect(usageWindowLabel({ trailingDays: 30 })).toBe('Last 30 days');
    expect(usageWindowLabel('lastMonth')).toBe('Last month');
    expect(usageWindowLabel({ trailingDays: 45 })).toBe('Custom');
  });

  test('offers every window the picker renders as a distinct key', () => {
    const keys = USAGE_WINDOWS.map((option) => usageWindowKey(option.window));
    expect(new Set(keys).size).toBe(USAGE_WINDOWS.length);
  });
});

describe('usage formatting', () => {
  test('keeps cents for small spend and drops them once it is not small', () => {
    expect(formatMoney(0.5)).toBe('$0.50');
    expect(formatMoney(9.99)).toBe('$9.99');
    expect(formatMoney(12.5)).toBe('$13');
    expect(formatMoney(1_234)).toBe('$1,234');
  });

  test('compacts token counts the way the desktop meter does', () => {
    expect(formatTokens(900)).toBe('900');
    expect(formatTokens(1_500)).toBe('1.5k');
    expect(formatTokens(1_234_000)).toBe('1.2M');
  });

  test('renders fractional shares as whole percents', () => {
    expect(formatPercent(0)).toBe('0%');
    expect(formatPercent(0.724)).toBe('72%');
    expect(formatPercent(1)).toBe('100%');
  });

  test('groups large counts', () => {
    expect(formatCount(42)).toBe('42');
    expect(formatCount(4_200)).toBe('4,200');
  });

  test('spans a range within one year without repeating it', () => {
    expect(formatUsageRange('2026-02-01', '2026-03-03', 'en-US')).toBe('Feb 1 – Mar 3');
  });

  test('includes the year when a range crosses one', () => {
    expect(formatUsageRange('2025-12-01', '2026-01-31', 'en-US')).toBe(
      'Dec 1, 2025 – Jan 31, 2026',
    );
  });
});

describe('plan reset labels', () => {
  const now = 1_800_000_000;

  test('never counts down past the reset', () => {
    expect(planResetLabel(now - 1, now)).toBe('Resets soon');
    expect(planResetLabel(now, now)).toBe('Resets soon');
  });

  test('counts minutes inside the hour', () => {
    expect(planResetLabel(now + 30 * 60, now)).toBe('Resets in 30m');
  });

  test('splits hours and minutes, and drops an empty remainder', () => {
    expect(planResetLabel(now + 90 * 60, now)).toBe('Resets in 1h 30m');
    expect(planResetLabel(now + 120 * 60, now)).toBe('Resets in 2h');
  });

  test('falls back to a weekday and time beyond a day', () => {
    expect(planResetLabel(now + 30 * 3_600, now, 'en-US')).toMatch(/^Resets \w{3} \d/);
  });
});

describe('usage breakdowns', () => {
  test('sorts providers by the metric on screen', () => {
    expect(sortedProviders(history(), 'cost').map((slice) => slice.provider)).toEqual([
      'codex',
      'claude',
    ]);
    expect(sortedProviders(history(), 'tokens').map((slice) => slice.provider)).toEqual([
      'claude',
      'codex',
    ]);
  });

  test('does not mutate the history it sorts', () => {
    const source = history();
    sortedProviders(source, 'cost');
    expect(source.providers[0]?.provider).toBe('codex');
  });

  test('tops models by spend, limited', () => {
    expect(topModels(history(), 2).map((slice) => slice.model)).toEqual(['gpt-5', 'opus-4']);
  });

  test('names providers the way the rest of the product does', () => {
    expect(usageProviderLabel('claude')).toBe('Claude Code');
    expect(usageProviderLabel('codex')).toBe('Codex');
  });

  test('summarizes what the scan read', () => {
    expect(scanSummary(history())).toBe('1,500 files scanned · 4,200 records · 1.3s');
  });
});

describe('window round trip', () => {
  test('matches the window the daemon echoes back', () => {
    const requested: UsageWindow = { trailingDays: 90 };
    const echoed: UsageWindow = { trailingDays: 90 };
    expect(usageWindowKey(echoed)).toBe(usageWindowKey(requested));
  });
});
