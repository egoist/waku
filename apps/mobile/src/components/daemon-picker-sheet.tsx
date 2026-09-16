import * as Haptics from 'expo-haptics';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ActivityIndicator,
  Alert,
  Animated,
  Easing,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
  type ColorValue,
  useWindowDimensions,
} from 'react-native';

import { AppSymbol } from '@/components/app-symbol';
import { DaemonList } from '@/components/daemon-list';
import { Sheet, SheetRow } from '@/components/sheet';
import { NativeTint, Radius } from '@/constants/theme';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';
import {
  isPrivateDaemonAddress,
  normalizeDaemonAddress,
  type DaemonProfile,
} from '@/lib/daemon-profile';

type EditorTarget = string | null | undefined;
type ConnectionSecurity = 'secure' | 'private' | 'insecure' | 'invalid' | null;

export function DaemonPickerSheet({
  onDismiss,
  visible,
}: {
  onDismiss: () => void;
  visible: boolean;
}) {
  const daemon = useDaemon();
  const reducedMotion = useReducedMotion();
  const { width } = useWindowDimensions();
  const [editorTarget, setEditorTarget] = useState<EditorTarget>(undefined);
  const [editorProfile, setEditorProfile] = useState<DaemonProfile | undefined>(undefined);
  const [pageWidth, setPageWidth] = useState(Math.max(1, width - 16));
  const pageProgress = useRef(new Animated.Value(0)).current;
  const navigationAnimation = useRef<ReturnType<typeof Animated.timing> | null>(null);

  useEffect(() => {
    if (!visible) return;
    const startsInEditor = daemon.profiles.length === 0;
    navigationAnimation.current?.stop();
    setEditorTarget(startsInEditor ? null : undefined);
    setEditorProfile(undefined);
    pageProgress.setValue(startsInEditor ? 1 : 0);
  }, [visible]);

  useEffect(() => () => navigationAnimation.current?.stop(), []);

  function pushEditor(target: string | null) {
    navigationAnimation.current?.stop();
    setEditorTarget(target);
    setEditorProfile(
      typeof target === 'string'
        ? daemon.profiles.find((item) => item.id === target)
        : undefined,
    );
    pageProgress.setValue(0);
    if (reducedMotion) {
      pageProgress.setValue(1);
      return;
    }
    const next = Animated.timing(pageProgress, {
      duration: 240,
      easing: Easing.out(Easing.cubic),
      toValue: 1,
      useNativeDriver: true,
    });
    navigationAnimation.current = next;
    next.start();
  }

  function popEditor() {
    if (editorTarget === undefined) return;
    navigationAnimation.current?.stop();
    if (reducedMotion) {
      pageProgress.setValue(0);
      setEditorTarget(undefined);
      setEditorProfile(undefined);
      return;
    }
    const next = Animated.timing(pageProgress, {
      duration: 240,
      easing: Easing.out(Easing.cubic),
      toValue: 0,
      useNativeDriver: true,
    });
    navigationAnimation.current = next;
    next.start(({ finished }) => {
      if (finished) {
        setEditorTarget(undefined);
        setEditorProfile(undefined);
      }
    });
  }

  function handleDismiss() {
    navigationAnimation.current?.stop();
    pageProgress.setValue(0);
    setEditorTarget(undefined);
    setEditorProfile(undefined);
    onDismiss();
  }

  function select(profileToSelect: DaemonProfile) {
    onDismiss();
    if (profileToSelect.id === daemon.activeProfile?.id) return;
    void Haptics.selectionAsync();
    void daemon.selectProfile(profileToSelect.id).then((connected) => {
      if (!connected) {
        void Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
      }
    });
  }

  const translateX = pageProgress.interpolate({
    inputRange: [0, 1],
    outputRange: [0, -pageWidth],
  });

  return (
    <Sheet onDismiss={handleDismiss} visible={visible}>
      <View
        onLayout={(event) => setPageWidth(Math.max(1, event.nativeEvent.layout.width))}
        style={styles.navigationViewport}>
        <Animated.View
          style={[
            styles.navigationPages,
            { transform: [{ translateX }], width: pageWidth * 2 },
          ]}>
          <View
            accessibilityElementsHidden={editorTarget !== undefined}
            importantForAccessibility={editorTarget === undefined ? 'auto' : 'no-hide-descendants'}
            pointerEvents={editorTarget === undefined ? 'auto' : 'none'}
            style={{ width: pageWidth }}>
            <SheetPageHeader title="Daemons" />
            <DaemonList
              onEdit={(item) => pushEditor(item.id)}
              onSelect={select}
            />
            <SheetRow
              description="Connect another host"
              label="Add Daemon…"
              leading={(
                <AppSymbol
                  name={{ ios: 'plus.circle', android: 'add_circle', web: 'add_circle' }}
                  size={22}
                  tintColor={NativeTint}
                />
              )}
              onPress={() => pushEditor(null)}
            />
          </View>
          <View
            accessibilityElementsHidden={editorTarget === undefined}
            importantForAccessibility={editorTarget === undefined ? 'no-hide-descendants' : 'auto'}
            pointerEvents={editorTarget === undefined ? 'none' : 'auto'}
            style={{ width: pageWidth }}>
            {editorTarget !== undefined && (
              <DaemonEditor
                key={editorProfile?.id ?? 'new'}
                profile={editorProfile}
                onBack={popEditor}
                onRemoved={popEditor}
                onSaved={popEditor}
              />
            )}
          </View>
        </Animated.View>
      </View>
    </Sheet>
  );
}

function DaemonEditor({
  onBack,
  onRemoved,
  onSaved,
  profile,
}: {
  onBack: () => void;
  onRemoved: () => void;
  onSaved: () => void;
  profile?: DaemonProfile;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  const [name, setName] = useState(profile?.name ?? '');
  const [address, setAddress] = useState(profile?.address ?? '');
  const [token, setToken] = useState('');
  const [revealed, setRevealed] = useState(false);
  const [saving, setSaving] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const addressInput = useRef<TextInput>(null);
  const tokenInput = useRef<TextInput>(null);
  const security = useMemo<ConnectionSecurity>(() => {
    if (!address.trim()) return null;
    try {
      const normalized = normalizeDaemonAddress(address);
      if (normalized.startsWith('wss://')) return 'secure';
      return isPrivateDaemonAddress(normalized) ? 'private' : 'insecure';
    } catch {
      return 'invalid';
    }
  }, [address]);
  const canSave = Boolean(
    address.trim()
      && (profile || token.trim())
      && security !== 'invalid'
      && security !== 'insecure'
      && !saving
      && !removing,
  );

  async function save() {
    if (!canSave) return;
    setSaving(true);
    setLocalError(null);
    try {
      const result = await daemon.saveProfile({ name, address, token }, profile?.id);
      await Haptics.notificationAsync(
        result.connected
          ? Haptics.NotificationFeedbackType.Success
          : Haptics.NotificationFeedbackType.Warning,
      );
      onSaved();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setSaving(false);
    }
  }

  async function remove() {
    if (!profile || saving || removing) return;
    setRemoving(true);
    setLocalError(null);
    try {
      await daemon.removeProfile(profile.id);
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
      onRemoved();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setRemoving(false);
    }
  }

  function confirmRemove() {
    if (!profile || saving || removing) return;
    Alert.alert(
      `Remove ${profile.name}?`,
      'This removes the saved address and token from this device. Tasks remain on the daemon host.',
      [
        { text: 'Cancel', style: 'cancel' },
        { text: 'Remove', style: 'destructive', onPress: () => void remove() },
      ],
    );
  }

  return (
    <View style={styles.editor}>
      <SheetPageHeader
        backDisabled={saving || removing}
        title={profile ? 'Edit Daemon' : 'Add Daemon'}
        onBack={onBack}
      />
      <View style={[styles.formGroup, { backgroundColor: theme.overlay }]}>
        <View style={styles.formRow}>
          <Text style={[styles.fieldLabel, { color: theme.text }]}>Name</Text>
          <TextInput
            accessibilityLabel="Daemon name"
            autoCapitalize="words"
            autoCorrect={false}
            editable={!saving && !removing}
            onChangeText={(value) => {
              setName(value);
              setLocalError(null);
            }}
            onSubmitEditing={() => addressInput.current?.focus()}
            placeholder="Optional"
            placeholderTextColor={theme.textTertiary}
            returnKeyType="next"
            selectionColor={NativeTint}
            style={[styles.rowInput, { color: theme.text }]}
            value={name}
          />
        </View>
        <View style={[styles.separator, { backgroundColor: theme.separator }]} />
        <View style={styles.formRow}>
          <Text style={[styles.fieldLabel, { color: theme.text }]}>Address</Text>
          <TextInput
            ref={addressInput}
            accessibilityLabel="Daemon WebSocket address"
            autoCapitalize="none"
            autoCorrect={false}
            clearButtonMode="while-editing"
            editable={!saving && !removing}
            inputMode="url"
            keyboardType="url"
            onChangeText={(value) => {
              setAddress(value);
              setLocalError(null);
            }}
            onSubmitEditing={() => tokenInput.current?.focus()}
            placeholder="wss://host.example"
            placeholderTextColor={theme.textTertiary}
            returnKeyType="next"
            selectionColor={NativeTint}
            spellCheck={false}
            style={[styles.rowInput, { color: theme.text }]}
            value={address}
          />
        </View>
        <View style={[styles.separator, { backgroundColor: theme.separator }]} />
        <View style={styles.formRow}>
          <Text style={[styles.fieldLabel, { color: theme.text }]}>Token</Text>
          <TextInput
            ref={tokenInput}
            accessibilityLabel="Daemon token"
            autoCapitalize="none"
            autoComplete="off"
            autoCorrect={false}
            editable={!saving && !removing}
            onChangeText={(value) => {
              setToken(value);
              setLocalError(null);
            }}
            onSubmitEditing={() => void save()}
            placeholder={profile ? 'Unchanged' : 'Required'}
            placeholderTextColor={theme.textTertiary}
            returnKeyType="done"
            secureTextEntry={!revealed}
            selectionColor={NativeTint}
            spellCheck={false}
            style={[styles.rowInput, styles.tokenInput, { color: theme.text }]}
            value={token}
          />
          <Pressable
            accessibilityLabel={revealed ? 'Hide token' : 'Reveal token'}
            accessibilityRole="button"
            accessibilityState={{ selected: revealed }}
            disabled={saving || removing}
            onPress={() => setRevealed((value) => !value)}
            style={({ pressed }) => [styles.revealButton, { opacity: pressed ? 0.45 : 1 }]}>
            <AppSymbol
              name={revealed
                ? { ios: 'eye.slash', android: 'visibility_off', web: 'visibility_off' }
                : { ios: 'eye', android: 'visibility', web: 'visibility' }}
              size={18}
              tintColor={theme.textSecondary}
            />
          </Pressable>
        </View>
      </View>

      <ConnectionFootnote editing={Boolean(profile)} security={security} />

      {localError && (
        <View accessibilityLiveRegion="polite" style={styles.messageRow}>
          <AppSymbol
            name={{ ios: 'exclamationmark.circle.fill', android: 'error', web: 'error' }}
            size={14}
            tintColor={theme.danger}
          />
          <Text style={[styles.messageText, { color: theme.danger }]}>{localError}</Text>
        </View>
      )}

      <View style={styles.editorButtons}>
        <Pressable
          accessibilityRole="button"
          accessibilityState={{ disabled: !canSave }}
          disabled={!canSave}
          onPress={() => void save()}
          style={({ pressed }) => [
            styles.editorButton,
            { backgroundColor: NativeTint, opacity: !canSave ? 0.35 : pressed ? 0.6 : 1 },
          ]}>
          {saving && <ActivityIndicator color="#ffffff" size="small" />}
          <Text style={[styles.editorButtonText, styles.saveButtonText]}>
            {saving ? 'Saving…' : profile ? 'Save' : 'Add'}
          </Text>
        </Pressable>
      </View>

      {profile && (
        <Pressable
          accessibilityRole="button"
          disabled={saving || removing}
          onPress={confirmRemove}
          style={({ pressed }) => [styles.removeButton, { opacity: pressed ? 0.5 : 1 }]}>
          {removing && <ActivityIndicator color={theme.danger} size="small" />}
          <Text style={[styles.removeText, { color: theme.danger }]}>
            {removing ? 'Removing…' : 'Remove Daemon'}
          </Text>
        </Pressable>
      )}
    </View>
  );
}

function SheetPageHeader({
  backDisabled = false,
  onBack,
  title,
}: {
  backDisabled?: boolean;
  onBack?: () => void;
  title: string;
}) {
  const theme = useTheme();
  return (
    <View style={styles.pageHeader}>
      {onBack ? (
        <Pressable
          accessibilityLabel="Back to daemons"
          accessibilityRole="button"
          accessibilityState={{ disabled: backDisabled }}
          disabled={backDisabled}
          hitSlop={8}
          onPress={onBack}
          style={({ pressed }) => [
            styles.backButton,
            { opacity: backDisabled ? 0.3 : pressed ? 0.45 : 1 },
          ]}>
          <AppSymbol
            name={{ ios: 'chevron.left', android: 'arrow_back', web: 'arrow_back' }}
            size={20}
            tintColor={NativeTint}
          />
        </Pressable>
      ) : (
        <View style={styles.headerSide} />
      )}
      <Text numberOfLines={1} style={[styles.pageTitle, { color: theme.text }]}>
        {title}
      </Text>
      <View style={styles.headerSide} />
    </View>
  );
}

function ConnectionFootnote({
  editing,
  security,
}: {
  editing: boolean;
  security: ConnectionSecurity;
}) {
  const theme = useTheme();
  let color: ColorValue = theme.textTertiary;
  let icon: Parameters<typeof AppSymbol>[0]['name'] = {
    ios: 'info.circle',
    android: 'info',
    web: 'info',
  };
  let message = editing
    ? 'Leave the token blank to keep the saved credential.'
    : 'Copy the address and token from Waku Desktop → Settings → Daemon.';

  if (security === 'invalid') {
    color = theme.danger;
    icon = { ios: 'exclamationmark.circle.fill', android: 'error', web: 'error' };
    message = 'Enter a valid ws:// or wss:// WebSocket address.';
  } else if (security === 'insecure') {
    color = theme.danger;
    icon = { ios: 'exclamationmark.shield.fill', android: 'gpp_bad', web: 'warning' };
    message = 'Public plaintext connections are blocked. Use wss:// for this host.';
  } else if (security === 'private') {
    color = theme.warning;
    icon = { ios: 'wifi', android: 'wifi', web: 'wifi' };
    message = 'Unencrypted connection. Use it only on a LAN or tailnet you trust.';
  } else if (security === 'secure') {
    color = theme.success;
    icon = { ios: 'lock.fill', android: 'lock', web: 'lock' };
    message = Platform.OS === 'web'
      ? 'Encrypted connection. The token stays in this browser.'
      : 'Encrypted connection. The token is stored in this device’s keychain.';
  }

  return (
    <View style={styles.messageRow}>
      <AppSymbol name={icon} size={15} tintColor={color} />
      <Text style={[styles.messageText, { color }]}>{message}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  navigationViewport: { alignSelf: 'stretch', overflow: 'hidden' },
  navigationPages: { alignItems: 'flex-start', flexDirection: 'row' },
  pageHeader: {
    alignItems: 'center',
    flexDirection: 'row',
    height: 46,
    marginBottom: 4,
  },
  headerSide: { width: 44 },
  backButton: { alignItems: 'center', height: 44, justifyContent: 'center', width: 44 },
  pageTitle: { flex: 1, fontSize: 17, fontWeight: '600', textAlign: 'center' },
  editor: { paddingBottom: 4 },
  formGroup: { borderRadius: Radius.medium, overflow: 'hidden' },
  formRow: {
    alignItems: 'center',
    flexDirection: 'row',
    minHeight: 54,
    paddingLeft: 12,
    paddingRight: 4,
  },
  fieldLabel: { fontSize: 15.5, width: 74 },
  rowInput: {
    flex: 1,
    fontSize: 15.5,
    minHeight: 53,
    paddingHorizontal: 8,
    paddingVertical: 11,
    textAlign: Platform.select({ ios: 'right', default: 'left' }),
  },
  tokenInput: { paddingRight: 2 },
  revealButton: { alignItems: 'center', height: 44, justifyContent: 'center', width: 42 },
  separator: { height: StyleSheet.hairlineWidth, marginLeft: 12 },
  messageRow: {
    alignItems: 'flex-start',
    flexDirection: 'row',
    gap: 7,
    marginHorizontal: 8,
    marginTop: 8,
  },
  messageText: { flex: 1, fontSize: 12.5, lineHeight: 17 },
  editorButtons: { flexDirection: 'row', gap: 8, marginTop: 16 },
  editorButton: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    flex: 1,
    flexDirection: 'row',
    gap: 7,
    justifyContent: 'center',
    minHeight: 48,
    paddingHorizontal: 14,
  },
  editorButtonText: { fontSize: 16, fontWeight: '600' },
  saveButtonText: { color: '#ffffff' },
  removeButton: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 7,
    justifyContent: 'center',
    minHeight: 46,
    marginTop: 4,
  },
  removeText: { fontSize: 15.5 },
});
