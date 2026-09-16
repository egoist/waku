import { router } from 'expo-router';
import { useEffect, useRef, useState } from 'react';
import {
  ActivityIndicator,
  Animated,
  Pressable,
  StyleSheet,
  Text,
  View,
} from 'react-native';

import { AppSymbol } from '@/components/app-symbol';
import { NativeTint, Radius, Spacing } from '@/constants/theme';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';

/** How long a dropped connection may stay quiet before it is announced; the
 * immediate retry usually lands well within this. */
const OUTAGE_GRACE_MS = 3_000;
/** How long the restored confirmation stays. */
const RESTORED_MS = 2_500;

export type ConnectionNotice =
  | {
      kind: 'reconnecting';
      title: string;
      detail: string | null;
      /** An attempt is in flight right now. */
      attempting: boolean;
    }
  | { kind: 'error'; title: string; detail: string }
  | { kind: 'restored'; title: string };

/**
 * What the connection banner should say, or null when the link needs no
 * words. An interrupted connection gets a grace period before it is
 * announced, so a blip the immediate retry heals never shows; a daemon that
 * has not been reached at all is announced at once, since nothing else on
 * screen explains the wait. A restored confirmation follows only a notice
 * that was actually on screen.
 */
export function useConnectionNotice(): ConnectionNotice | null {
  const daemon = useDaemon();
  const name = daemon.activeProfile?.name ?? 'the daemon';
  const outage = daemon.phase === 'reconnecting' ? daemon.outage : null;
  const [restoredUntil, setRestoredUntil] = useState<number | null>(null);
  const shown = useRef(false);

  const now = Date.now();
  const announceAt = outage ? outage.since + (outage.interrupted ? OUTAGE_GRACE_MS : 0) : null;
  const visibleOutage = outage !== null && announceAt !== null && now >= announceAt;
  const countdown = visibleOutage && outage.nextRetryAt !== null;
  useWakeups(countdown ? 1_000 : null, [
    announceAt !== null && now < announceAt ? announceAt : null,
    restoredUntil,
  ]);

  useEffect(() => {
    if (daemon.phase === 'error' || visibleOutage) {
      shown.current = true;
    } else if (daemon.phase === 'connected') {
      if (shown.current) {
        shown.current = false;
        setRestoredUntil(Date.now() + RESTORED_MS);
      }
    } else if (daemon.phase !== 'reconnecting') {
      // A fresh selection or a manual disconnect owes nobody a confirmation.
      shown.current = false;
    }
  }, [daemon.phase, visibleOutage]);

  if (daemon.phase === 'error') {
    return {
      kind: 'error',
      title: `Can’t connect to ${name}`,
      detail: daemon.error ?? 'Something went wrong.',
    };
  }
  if (visibleOutage) {
    const attempting = outage.nextRetryAt === null;
    const seconds = attempting ? 0 : Math.max(1, Math.ceil((outage.nextRetryAt! - now) / 1_000));
    const progress = attempting
      ? outage.attempts === 0 ? null : 'Trying again…'
      : `Retrying in ${seconds}s`;
    const reason = outage.attempts > 0 ? outage.reason : null;
    return {
      kind: 'reconnecting',
      title: outage.interrupted ? `Reconnecting to ${name}…` : `Can’t reach ${name}`,
      detail: [progress, reason].filter(Boolean).join(' · ') || null,
      attempting,
    };
  }
  if (daemon.phase === 'connected' && restoredUntil !== null && now < restoredUntil) {
    return { kind: 'restored', title: `Reconnected to ${name}` };
  }
  return null;
}

/** Re-renders on an interval while one is set, otherwise once at the
 * earliest future moment given, so timed copy updates without polling. */
function useWakeups(intervalMs: number | null, moments: Array<number | null>): void {
  const [, setTick] = useState(0);
  const current = Date.now();
  let nextWake: number | null = null;
  for (const moment of moments) {
    if (moment === null || moment <= current) continue;
    if (nextWake === null || moment < nextWake) nextWake = moment;
  }
  useEffect(() => {
    if (intervalMs !== null) {
      const timer = setInterval(() => setTick((tick) => tick + 1), intervalMs);
      return () => clearInterval(timer);
    }
    if (nextWake === null) return;
    const timer = setTimeout(() => setTick((tick) => tick + 1), Math.max(0, nextWake - Date.now()));
    return () => clearTimeout(timer);
  }, [intervalMs, nextWake]);
}

/**
 * The connection's state of health, wherever a screen needs to explain a
 * stalled daemon: reconnecting progress with a way to retry now, a hard
 * failure with a way to fix the connection, and a brief confirmation once
 * the link is back. Inline it sits in a list; floating it hovers over
 * content on an opaque surface.
 */
export function ConnectionBanner({ floating = false }: { floating?: boolean }) {
  const notice = useConnectionNotice();
  if (!notice) return null;
  return <NoticeCard floating={floating} notice={notice} />;
}

function NoticeCard({ floating, notice }: { floating: boolean; notice: ConnectionNotice }) {
  const theme = useTheme();
  const daemon = useDaemon();
  const reducedMotion = useReducedMotion();
  const opacity = useRef(new Animated.Value(reducedMotion ? 1 : 0)).current;
  useEffect(() => {
    Animated.timing(opacity, {
      duration: reducedMotion ? 0 : 160,
      toValue: 1,
      useNativeDriver: true,
    }).start();
  }, [opacity, reducedMotion]);

  const tint = notice.kind === 'error'
    ? theme.danger
    : notice.kind === 'restored' ? theme.success : theme.warning;
  const soft = notice.kind === 'error'
    ? theme.dangerSoft
    : notice.kind === 'restored' ? theme.successSoft : theme.warningSoft;
  const detail = notice.kind === 'restored' ? null : notice.detail;

  return (
    <Animated.View
      accessibilityLiveRegion="polite"
      accessibilityRole="alert"
      style={[
        styles.card,
        floating
          ? [styles.floating, { backgroundColor: theme.surface, borderColor: theme.borderStrong }]
          : [styles.inline, { backgroundColor: soft }],
        { opacity },
      ]}>
      <View style={styles.icon}>
        {notice.kind === 'reconnecting' && notice.attempting ? (
          <ActivityIndicator color={tint} size="small" style={styles.spinner} />
        ) : (
          <AppSymbol name={noticeIcon(notice)} size={18} tintColor={tint} />
        )}
      </View>
      <View style={styles.copy}>
        <Text style={[styles.title, { color: theme.text }]}>{notice.title}</Text>
        {detail ? (
          <Text style={[styles.body, { color: theme.textSecondary }]}>{detail}</Text>
        ) : null}
      </View>
      {notice.kind === 'reconnecting' && (
        <BannerAction
          disabled={notice.attempting}
          label="Retry now"
          accessibilityLabel="Retry daemon connection now"
          onPress={() => void daemon.reconnect()}
        />
      )}
      {notice.kind === 'error' && (
        <View style={styles.actions}>
          <BannerAction
            label="Retry"
            accessibilityLabel="Retry daemon connection"
            onPress={() => void daemon.reconnect()}
          />
          {daemon.activeProfile && (
            <BannerAction
              label="Edit"
              accessibilityLabel="Edit daemon connection"
              onPress={() => router.push({
                pathname: '/daemon-editor',
                params: { id: daemon.activeProfile!.id },
              })}
            />
          )}
        </View>
      )}
    </Animated.View>
  );
}

function noticeIcon(notice: ConnectionNotice): Parameters<typeof AppSymbol>[0]['name'] {
  switch (notice.kind) {
    case 'error':
      return { ios: 'exclamationmark.triangle.fill', android: 'warning', web: 'warning' };
    case 'restored':
      return { ios: 'checkmark.circle.fill', android: 'check_circle', web: 'check_circle' };
    default:
      return { ios: 'wifi.slash', android: 'wifi_off', web: 'wifi_off' };
  }
}

function BannerAction({
  label,
  accessibilityLabel,
  disabled = false,
  onPress,
}: {
  label: string;
  accessibilityLabel: string;
  disabled?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityLabel={accessibilityLabel}
      accessibilityRole="button"
      accessibilityState={{ disabled }}
      disabled={disabled}
      hitSlop={8}
      onPress={onPress}
      style={({ pressed }) => [styles.action, { opacity: pressed || disabled ? 0.45 : 1 }]}>
      <Text style={[styles.actionText, { color: NativeTint }]}>{label}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  card: {
    alignItems: 'center',
    borderRadius: Radius.large,
    flexDirection: 'row',
    gap: 10,
    paddingHorizontal: 14,
    paddingVertical: 12,
  },
  inline: {
    marginBottom: 10,
    marginHorizontal: Spacing.three,
  },
  floating: {
    borderWidth: StyleSheet.hairlineWidth,
  },
  icon: { alignItems: 'center', justifyContent: 'center', width: 18 },
  spinner: { height: 18, transform: [{ scale: 0.8 }], width: 18 },
  copy: { flex: 1 },
  title: { fontSize: 14, fontWeight: '700' },
  body: { fontSize: 12, lineHeight: 17, marginTop: 2 },
  actions: { alignItems: 'center', flexDirection: 'row', gap: 12 },
  action: { justifyContent: 'center', minHeight: 32, paddingHorizontal: 4 },
  actionText: { fontSize: 13, fontWeight: '700' },
});
