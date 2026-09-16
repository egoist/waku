import * as Haptics from "expo-haptics";
import type { SymbolViewProps } from "expo-symbols";
import { router, Stack, useLocalSearchParams } from "expo-router";
import { navigateBack } from "@/components/screen-header";
import { useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  Alert,
  Platform,
  PlatformColor,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
  type ColorValue,
} from "react-native";

import { AppSymbol } from "@/components/app-symbol";
import { Radius } from "@/constants/theme";
import { useTheme } from "@/hooks/use-theme";
import { useDaemon } from "@/lib/daemon-context";
import {
  isPrivateDaemonAddress,
  normalizeDaemonAddress,
} from "@/lib/daemon-profile";

export default function DaemonEditorScreen() {
  const colors = useNativeFormColors();
  const params = useLocalSearchParams<{ id?: string | string[] }>();
  const profileId = Array.isArray(params.id) ? params.id[0] : params.id;
  const daemon = useDaemon();
  const profile = daemon.profiles.find((item) => item.id === profileId);
  const [name, setName] = useState(profile?.name ?? "");
  const [address, setAddress] = useState(profile?.address ?? "");
  const [token, setToken] = useState("");
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
      if (normalized.startsWith("wss://")) return "secure";
      return isPrivateDaemonAddress(normalized) ? "private" : "insecure";
    } catch {
      return "invalid";
    }
  }, [address]);
  const canSave = Boolean(
    address.trim() &&
    (profile || token.trim()) &&
    security !== "invalid" &&
    security !== "insecure" &&
    !saving &&
    !removing,
  );

  async function save() {
    if (!canSave) return;
    setSaving(true);
    setLocalError(null);
    try {
      const result = await daemon.saveProfile(
        { name, address, token },
        profile?.id,
      );
      await Haptics.notificationAsync(
        result.connected
          ? Haptics.NotificationFeedbackType.Success
          : Haptics.NotificationFeedbackType.Warning,
      );
      navigateBack();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setSaving(false);
    }
  }

  async function remove() {
    if (!profile || removing || saving) return;
    setRemoving(true);
    setLocalError(null);
    try {
      await daemon.removeProfile(profile.id);
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
      navigateBack();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setRemoving(false);
    }
  }

  function confirmRemove() {
    if (!profile || removing || saving) return;
    Alert.alert(
      `Remove ${profile.name}?`,
      "This removes the saved address and token from this device. Tasks remain on the daemon host.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Remove",
          style: "destructive",
          onPress: () => void remove(),
        },
      ],
    );
  }

  return (
    <View style={[styles.screen, { backgroundColor: colors.background }]}>
      <Stack.Screen
        options={{
          title: profile ? "Edit Daemon" : "Add Daemon",
          contentStyle: { backgroundColor: colors.background },
          headerBackVisible: false,
          headerStyle: { backgroundColor: colors.background },
          headerTintColor: colors.accent,
          headerTitleStyle: { color: colors.text },
          gestureEnabled: !saving && !removing,
          ...(Platform.OS === "ios"
            ? {
                unstable_headerLeftItems: () => [
                  {
                    type: "button" as const,
                    label: "Cancel",
                    disabled: saving || removing,
                    onPress: navigateBack,
                  },
                ],
                unstable_headerRightItems: () => [
                  {
                    type: "button" as const,
                    label: saving ? "Saving…" : profile ? "Save" : "Add",
                    variant: "done" as const,
                    disabled: !canSave,
                    onPress: () => void save(),
                  },
                ],
              }
            : {
                headerLeft: () => (
                  <HeaderButton
                    disabled={saving || removing}
                    label="Cancel"
                    onPress={navigateBack}
                  />
                ),
                headerRight: () => (
                  <HeaderButton
                    disabled={!canSave}
                    emphasized
                    label={saving ? "Saving…" : profile ? "Save" : "Add"}
                    onPress={() => void save()}
                  />
                ),
              }),
        }}
      />
      <ScrollView
        automaticallyAdjustKeyboardInsets
        contentContainerStyle={styles.content}
        contentInsetAdjustmentBehavior="automatic"
        keyboardDismissMode="interactive"
        keyboardShouldPersistTaps="handled"
        showsVerticalScrollIndicator={false}
      >
        <Text style={[styles.sectionLabel, { color: colors.secondaryText }]}>
          CONNECTION
        </Text>
        <View style={[styles.formGroup, { backgroundColor: colors.surface }]}>
          <View style={styles.formRow}>
            <Text style={[styles.fieldLabel, { color: colors.text }]}>
              Name
            </Text>
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
              placeholderTextColor={colors.placeholder}
              returnKeyType="next"
              selectionColor={colors.accent}
              style={[styles.rowInput, { color: colors.text }]}
              value={name}
            />
          </View>

          <View
            style={[styles.separator, { backgroundColor: colors.separator }]}
          />
          <View style={styles.formRow}>
            <Text style={[styles.fieldLabel, { color: colors.text }]}>
              Address
            </Text>
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
              placeholderTextColor={colors.placeholder}
              returnKeyType="next"
              selectionColor={colors.accent}
              spellCheck={false}
              style={[styles.rowInput, { color: colors.text }]}
              value={address}
            />
          </View>

          <View
            style={[styles.separator, { backgroundColor: colors.separator }]}
          />
          <View style={styles.formRow}>
            <Text style={[styles.fieldLabel, { color: colors.text }]}>
              Token
            </Text>
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
              placeholder={profile ? "Unchanged" : "Required"}
              placeholderTextColor={colors.placeholder}
              returnKeyType="done"
              secureTextEntry={!revealed}
              selectionColor={colors.accent}
              spellCheck={false}
              style={[
                styles.rowInput,
                styles.tokenText,
                { color: colors.text },
              ]}
              value={token}
              onSubmitEditing={() => void save()}
            />
            <Pressable
              accessibilityLabel={revealed ? "Hide token" : "Reveal token"}
              accessibilityRole="button"
              accessibilityState={{ selected: revealed }}
              disabled={saving || removing}
              onPress={() => setRevealed((value) => !value)}
              style={({ pressed }) => [
                styles.revealButton,
                { opacity: pressed ? 0.45 : 1 },
              ]}
            >
              <AppSymbol
                name={
                  revealed
                    ? {
                        ios: "eye.slash",
                        android: "visibility_off",
                        web: "visibility_off",
                      }
                    : { ios: "eye", android: "visibility", web: "visibility" }
                }
                size={18}
                tintColor={colors.secondaryText}
              />
            </Pressable>
          </View>
        </View>

        <ConnectionFootnote profile={Boolean(profile)} security={security} />

        {localError && (
          <View accessibilityLiveRegion="polite" style={styles.messageRow}>
            <AppSymbol
              name={{
                ios: "exclamationmark.circle.fill",
                android: "error",
                web: "error",
              }}
              size={14}
              tintColor={colors.danger}
            />
            <Text style={[styles.messageText, { color: colors.danger }]}>
              {localError}
            </Text>
          </View>
        )}

        {profile && (
          <>
            <Text
              style={[
                styles.sectionLabel,
                styles.actionsLabel,
                { color: colors.secondaryText },
              ]}
            >
              ACTIONS
            </Text>
            <View
              style={[styles.formGroup, { backgroundColor: colors.surface }]}
            >
              <Pressable
                accessibilityRole="button"
                disabled={removing || saving}
                onPress={confirmRemove}
                style={({ pressed }) => [
                  styles.removeRow,
                  { opacity: removing || saving || pressed ? 0.45 : 1 },
                ]}
              >
                {removing && (
                  <ActivityIndicator color={colors.danger} size="small" />
                )}
                <Text style={[styles.removeText, { color: colors.danger }]}>
                  {removing ? "Removing…" : "Remove Daemon"}
                </Text>
              </Pressable>
            </View>
            <Text
              style={[styles.actionFootnote, { color: colors.secondaryText }]}
            >
              Removes the saved address and token from this device. Tasks remain
              on the daemon host.
            </Text>
          </>
        )}
      </ScrollView>
    </View>
  );
}

type ConnectionSecurity = "secure" | "private" | "insecure" | "invalid" | null;

function ConnectionFootnote({
  profile,
  security,
}: {
  profile: boolean;
  security: ConnectionSecurity;
}) {
  const colors = useNativeFormColors();
  let color = colors.secondaryText;
  let icon: SymbolViewProps["name"] = {
    ios: "info.circle",
    android: "info",
    web: "info",
  };
  let text = profile
    ? "Leave the token blank to keep the saved credential."
    : "Copy the address and token from Waku Desktop → Settings → Daemon.";

  if (security === "invalid") {
    color = colors.danger;
    icon = {
      ios: "exclamationmark.circle.fill",
      android: "error",
      web: "error",
    };
    text = "Enter a valid ws:// or wss:// WebSocket address.";
  } else if (security === "insecure") {
    color = colors.danger;
    icon = {
      ios: "exclamationmark.shield.fill",
      android: "gpp_bad",
      web: "warning",
    };
    text =
      "Public plaintext connections are blocked. Use wss:// for this host.";
  } else if (security === "private") {
    color = colors.warning;
    icon = { ios: "wifi", android: "wifi", web: "wifi" };
    text = "Unencrypted connection. Use it only on a LAN or tailnet you trust.";
  } else if (security === "secure") {
    color = colors.success;
    icon = { ios: "lock.fill", android: "lock", web: "lock" };
    text = Platform.select({
      web: "Encrypted connection. The token stays in this browser.",
      default:
        "Encrypted connection. The token is stored in this device’s keychain.",
    });
  }

  return (
    <View style={styles.messageRow}>
      <AppSymbol name={icon} size={16} tintColor={color} />
      <Text style={[styles.messageText, { color }]}>{text}</Text>
    </View>
  );
}

function HeaderButton({
  disabled,
  emphasized = false,
  label,
  onPress,
}: {
  disabled: boolean;
  emphasized?: boolean;
  label: string;
  onPress: () => void;
}) {
  const colors = useNativeFormColors();
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ disabled }}
      disabled={disabled}
      hitSlop={8}
      onPress={onPress}
      style={({ pressed }) => [
        styles.headerButton,
        { opacity: disabled ? 0.35 : pressed ? 0.5 : 1 },
      ]}
    >
      <Text
        style={[
          styles.headerButtonText,
          emphasized && styles.headerButtonEmphasized,
          { color: colors.accent },
        ]}
      >
        {label}
      </Text>
    </Pressable>
  );
}

interface NativeFormColors {
  accent: ColorValue;
  background: ColorValue;
  danger: ColorValue;
  placeholder: ColorValue;
  secondaryText: ColorValue;
  separator: ColorValue;
  success: ColorValue;
  surface: ColorValue;
  text: ColorValue;
  warning: ColorValue;
}

function useNativeFormColors(): NativeFormColors {
  const theme = useTheme();
  if (Platform.OS === "ios") {
    return {
      accent: PlatformColor("systemBlue"),
      background: PlatformColor("systemGroupedBackground"),
      danger: PlatformColor("systemRed"),
      placeholder: PlatformColor("placeholderText"),
      secondaryText: PlatformColor("secondaryLabel"),
      separator: PlatformColor("separator"),
      success: PlatformColor("systemGreen"),
      surface: PlatformColor("secondarySystemGroupedBackground"),
      text: PlatformColor("label"),
      warning: PlatformColor("systemOrange"),
    };
  }
  return {
    accent: theme.accent,
    background: theme.background,
    danger: theme.danger,
    placeholder: theme.textTertiary,
    secondaryText: theme.textSecondary,
    separator: theme.separator,
    success: theme.success,
    surface: theme.surface,
    text: theme.text,
    warning: theme.warning,
  };
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  content: { paddingBottom: 44, paddingTop: 24 },
  sectionLabel: {
    fontSize: 13,
    fontWeight: "500",
    letterSpacing: 0.1,
    marginBottom: 7,
    marginHorizontal: 20,
  },
  actionsLabel: { marginTop: 28 },
  formGroup: {
    borderRadius: Platform.select({ ios: 12, default: Radius.medium }),
    marginHorizontal: 16,
    overflow: "hidden",
  },
  formRow: {
    alignItems: "center",
    flexDirection: "row",
    minHeight: 56,
    paddingLeft: 16,
    paddingRight: 8,
  },
  fieldLabel: { fontSize: 16, width: 82 },
  rowInput: {
    flex: 1,
    fontSize: 16,
    minHeight: 55,
    paddingHorizontal: 8,
    paddingVertical: 12,
    textAlign: Platform.select({ ios: "right", default: "left" }),
  },
  tokenText: { paddingRight: 2 },
  revealButton: {
    alignItems: "center",
    height: 44,
    justifyContent: "center",
    width: 44,
  },
  separator: { height: StyleSheet.hairlineWidth, marginLeft: 16 },
  messageRow: {
    alignItems: "flex-start",
    flexDirection: "row",
    gap: 7,
    marginHorizontal: 20,
    marginTop: 8,
  },
  messageText: { flex: 1, fontSize: 13, lineHeight: 18 },
  removeRow: {
    alignItems: "center",
    flexDirection: "row",
    gap: 8,
    justifyContent: "center",
    minHeight: 52,
    paddingHorizontal: 16,
  },
  removeText: { fontSize: 16 },
  actionFootnote: {
    fontSize: 13,
    lineHeight: 18,
    marginHorizontal: 20,
    marginTop: 8,
  },
  headerButton: { justifyContent: "center", minHeight: 44, minWidth: 44 },
  headerButtonText: { fontSize: 16 },
  headerButtonEmphasized: { fontWeight: "700" },
});
