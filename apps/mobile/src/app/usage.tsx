import type { PlanUsage, ProviderKind, UsageWindow } from '@waku/client';
import { Stack } from 'expo-router';
import { useMemo, useState } from 'react';
import {
  ActivityIndicator,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from 'react-native';

import { AppPressable } from '@/components/app-pressable';

import { AppSymbol } from '@/components/app-symbol';
import { ConnectionBanner } from '@/components/connection-banner';
import { ProviderIcon } from '@/components/provider-icon';
import { Sheet, SheetRow } from '@/components/sheet';
import { Radius, Spacing } from '@/constants/theme';
import {
  usePlanUsages,
  useProviderCatalog,
  useTaskState,
  useUsageHistory,
} from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import {
  USAGE_WINDOWS,
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
} from '@/lib/usage-presentation';

export default function UsageScreen() {
  const theme = useTheme();
  const taskState = useTaskState();
  const catalog = useProviderCatalog();
  const [window, setWindow] = useState<UsageWindow>({ trailingDays: 30 });
  const [pickerOpen, setPickerOpen] = useState(false);
  const projects = taskState.data?.projects ?? [];
  const usage = useUsageHistory(window, projects);
  const installedProviders = useMemo(
    () => catalog.providers.filter((item) => item.installed).map((item) => item.id),
    [catalog.providers],
  );
  const plans = usePlanUsages(installedProviders);

  // Keep the previous scan through a reconnect refetch, but never show one
  // window's numbers under another window's heading.
  const scanned = usage.data;
  const history = scanned && (!usage.isFetching
    || usageWindowKey(scanned.window) === usageWindowKey(window))
    ? scanned
    : undefined;

  const rescan = () => void usage.refetch();

  return (
    <View style={[styles.screen, { backgroundColor: theme.background }]}>
      <Stack.Screen
        options={{
          headerRight: () => (
            <AppPressable
              accessibilityLabel="Rescan usage"
              accessibilityRole="button"
              hitSlop={10}
              onPress={rescan}>
              <AppSymbol
                name={{ ios: 'arrow.clockwise', android: 'refresh', web: 'refresh' }}
                size={20}
                tintColor={theme.text}
              />
            </AppPressable>
          ),
          unstable_headerRightItems: () => [{
            type: 'button',
            accessibilityLabel: 'Rescan usage',
            icon: { type: 'sfSymbol', name: 'arrow.clockwise' },
            label: 'Rescan',
            onPress: rescan,
          }],
        }}
      />
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        contentContainerStyle={styles.content}>
        <ConnectionBanner />

        <AppPressable
          accessibilityLabel={`Usage window: ${usageWindowLabel(window)}`}
          accessibilityRole="button"
          onPress={() => setPickerOpen(true)}
          style={({ pressed }) => [
            styles.picker,
            {
              backgroundColor: theme.surface,
              borderColor: theme.border,
              opacity: pressed ? 0.6 : 1,
            },
          ]}>
          <AppSymbol
            name={{ ios: 'calendar', android: 'calendar_month', web: 'calendar_month' }}
            size={18}
            tintColor={theme.textSecondary}
          />
          <Text style={[styles.pickerLabel, { color: theme.textSecondary }]}>Window</Text>
          <Text style={[styles.pickerValue, { color: theme.text }]}>
            {usageWindowLabel(window)}
          </Text>
          <AppSymbol
            name={{ ios: 'chevron.up.chevron.down', android: 'unfold_more', web: 'unfold_more' }}
            size={13}
            tintColor={theme.textTertiary}
          />
        </AppPressable>

        {usage.error && (
          <Text style={[styles.notice, { color: theme.danger }]}>
            {usage.error instanceof Error ? usage.error.message : String(usage.error)}
          </Text>
        )}

        {!history ? (
          <View style={styles.loading}>
            <ActivityIndicator color={theme.textTertiary} />
            <Text style={[styles.loadingLabel, { color: theme.textTertiary }]}>
              Scanning provider transcripts…
            </Text>
          </View>
        ) : (
          <>
            <Text style={[styles.range, { color: theme.textTertiary }]}>
              {formatUsageRange(history.sinceDay, history.untilDay)}
            </Text>

            <View
              style={[
                styles.card,
                { backgroundColor: theme.surface, borderColor: theme.border },
              ]}>
              <Text style={[styles.cardLabel, { color: theme.textTertiary }]}>Spend</Text>
              <Text style={[styles.cardValue, { color: theme.text }]}>
                {formatMoney(history.costUsd)}
              </Text>
              <View style={styles.cardRow}>
                <Text style={[styles.cardDetail, { color: theme.textSecondary }]}>
                  {formatTokens(history.totalTokens)} tokens
                </Text>
                <Text style={[styles.cardDetail, { color: theme.textSecondary }]}>
                  {history.sessions === 1 ? '1 session' : `${history.sessions} sessions`}
                </Text>
              </View>
            </View>

            {history.errors.length > 0 && (
              <Text style={[styles.notice, { color: theme.danger }]}>
                {history.errors.join('\n')}
              </Text>
            )}
            {history.pricing === 'unavailable' && (
              <Text style={[styles.notice, { color: theme.warning }]}>
                Model rates are unavailable, so costs are incomplete.
              </Text>
            )}

            <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>Providers</Text>
            <View
              style={[
                styles.card,
                { backgroundColor: theme.surface, borderColor: theme.border },
              ]}>
              {history.providers.length ? (
                sortedProviders(history, 'cost').map((slice) => (
                  <View
                    accessibilityLabel={
                      `${usageProviderLabel(slice.provider)} ${formatPercent(slice.costShare)} of spend`
                    }
                    key={slice.provider}
                    style={styles.provider}>
                    <View style={styles.providerHead}>
                      <ProviderIcon provider={slice.provider} size={14} />
                      <Text
                        numberOfLines={1}
                        style={[styles.providerName, { color: theme.text }]}>
                        {usageProviderLabel(slice.provider)}
                      </Text>
                      <Text style={[styles.value, { color: theme.text }]}>
                        {formatMoney(slice.costUsd)}
                      </Text>
                    </View>
                    <View style={[styles.track, { backgroundColor: theme.overlayStrong }]}>
                      <View
                        style={[
                          styles.fill,
                          {
                            backgroundColor: theme.textSecondary,
                            width: `${Math.max(2, Math.min(100, slice.costShare * 100))}%`,
                          },
                        ]}
                      />
                    </View>
                    <Text style={[styles.detail, { color: theme.textTertiary }]}>
                      {formatPercent(slice.costShare)} of spend · {formatTokens(slice.totalTokens)} tokens
                    </Text>
                  </View>
                ))
              ) : (
                <Text style={[styles.empty, { color: theme.textTertiary }]}>
                  No activity in this window.
                </Text>
              )}
            </View>

            {history.models.length > 0 && (
              <>
                <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
                  Top models
                </Text>
                <View
                  style={[
                    styles.card,
                    { backgroundColor: theme.surface, borderColor: theme.border },
                  ]}>
                  {topModels(history).map((slice) => (
                    <View key={`${slice.provider}:${slice.model}`} style={styles.modelRow}>
                      <ProviderIcon provider={slice.provider} size={13} />
                      <Text
                        numberOfLines={1}
                        style={[styles.modelName, { color: theme.text }]}>
                        {slice.model}
                      </Text>
                      <Text style={[styles.value, { color: theme.text }]}>
                        {formatMoney(slice.costUsd)}
                      </Text>
                      <Text style={[styles.share, { color: theme.textTertiary }]}>
                        {formatPercent(slice.costShare)}
                      </Text>
                    </View>
                  ))}
                </View>
              </>
            )}

            {plans.some((entry) => entry.plan || entry.isFetching) && (
              <>
                <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
                  Plan limits
                </Text>
                {plans.map((entry) => (
                  entry.plan || entry.isFetching ? (
                    <PlanCard
                      error={entry.error}
                      isFetching={entry.isFetching}
                      key={entry.provider}
                      plan={entry.plan}
                      provider={entry.provider}
                    />
                  ) : null
                ))}
              </>
            )}

            <Text style={[styles.footer, { color: theme.textGhost }]}>
              {scanSummary(history)}
            </Text>
          </>
        )}
      </ScrollView>

      <Sheet onDismiss={() => setPickerOpen(false)} title="Window" visible={pickerOpen}>
        {USAGE_WINDOWS.map((option) => (
          <SheetRow
            key={usageWindowKey(option.window)}
            label={option.label}
            onPress={() => {
              setWindow(option.window);
              setPickerOpen(false);
            }}
            selected={usageWindowKey(option.window) === usageWindowKey(window)}
          />
        ))}
      </Sheet>
    </View>
  );
}

function PlanCard({
  error,
  isFetching,
  plan,
  provider,
}: {
  error: unknown;
  isFetching: boolean;
  plan: PlanUsage | null;
  provider: ProviderKind;
}) {
  const theme = useTheme();
  return (
    <View style={[styles.card, { backgroundColor: theme.surface, borderColor: theme.border }]}>
      <View style={styles.planHead}>
        <ProviderIcon provider={provider} size={14} />
        <Text numberOfLines={1} style={[styles.providerName, { color: theme.text }]}>
          {plan?.planLabel ?? 'Plan'}
        </Text>
        {isFetching && <ActivityIndicator color={theme.textTertiary} size="small" />}
      </View>
      {error ? (
        <Text style={[styles.detail, { color: theme.danger }]}>
          {error instanceof Error ? error.message : String(error)}
        </Text>
      ) : null}
      {plan?.windows.map((lane) => (
        <View key={`${lane.label}-${lane.resetsAt ?? 'none'}`} style={styles.lane}>
          <View style={styles.laneHead}>
            <Text numberOfLines={1} style={[styles.laneLabel, { color: theme.text }]}>
              {lane.label}
            </Text>
            {lane.resetsAt != null && (
              <Text numberOfLines={1} style={[styles.detail, { color: theme.textTertiary }]}>
                {planResetLabel(lane.resetsAt)}
              </Text>
            )}
            <Text style={[styles.value, { color: theme.textSecondary }]}>
              {Math.round(lane.percent)}%
            </Text>
          </View>
          <View style={[styles.track, { backgroundColor: theme.overlayStrong }]}>
            <View
              style={[
                styles.fill,
                {
                  backgroundColor: lane.percent >= 95
                    ? theme.danger
                    : lane.percent >= 80
                      ? theme.warning
                      : theme.textSecondary,
                  width: `${Math.max(2, Math.min(100, lane.percent))}%`,
                },
              ]}
            />
          </View>
        </View>
      ))}
      {plan && !plan.windows.length && (
        <Text style={[styles.detail, { color: theme.textTertiary }]}>
          No plan windows reported.
        </Text>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  content: { paddingBottom: 36, paddingHorizontal: Spacing.three },
  picker: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    borderWidth: StyleSheet.hairlineWidth,
    flexDirection: 'row',
    gap: 10,
    marginTop: Spacing.two,
    minHeight: 46,
    paddingHorizontal: 12,
  },
  pickerLabel: { fontSize: 14 },
  pickerValue: { flex: 1, fontSize: 15, fontWeight: '600' },
  range: { fontSize: 12.5, marginLeft: 12, marginTop: 8 },
  notice: { fontSize: 12.5, lineHeight: 17, marginHorizontal: 12, marginTop: 8 },
  loading: { alignItems: 'center', gap: 10, paddingTop: 60 },
  loadingLabel: { fontSize: 13 },
  card: {
    borderWidth: StyleSheet.hairlineWidth,
    borderRadius: Radius.medium,
    marginTop: Spacing.two,
    padding: 14,
  },
  cardLabel: {
    fontSize: 11,
    fontWeight: '600',
    letterSpacing: 0.4,
    textTransform: 'uppercase',
  },
  cardValue: { fontSize: 30, fontWeight: '500', marginTop: 2 },
  cardRow: { flexDirection: 'row', gap: 14, marginTop: 4 },
  cardDetail: { fontSize: 12.5 },
  sectionTitle: {
    fontSize: 13,
    fontWeight: '500',
    marginBottom: 7,
    marginLeft: 12,
    marginTop: 18,
    textTransform: 'uppercase',
  },
  provider: { marginTop: 14 },
  providerHead: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  providerName: { flex: 1, fontSize: 14, fontWeight: '500' },
  value: { fontSize: 14, fontWeight: '600' },
  share: { fontSize: 12, minWidth: 38, textAlign: 'right' },
  detail: { fontSize: 11.5, marginTop: 4 },
  track: {
    borderRadius: Radius.pill,
    height: 4,
    marginTop: 7,
    overflow: 'hidden',
  },
  fill: { borderRadius: Radius.pill, height: 4 },
  modelRow: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 8,
    paddingVertical: 6,
  },
  modelName: { flex: 1, fontSize: 13.5 },
  planHead: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  lane: { marginTop: 12 },
  laneHead: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  laneLabel: { flexShrink: 1, fontSize: 13.5, fontWeight: '500' },
  empty: { fontSize: 13, paddingVertical: 6 },
  footer: { fontSize: 11, marginLeft: 12, marginTop: 14 },
});
