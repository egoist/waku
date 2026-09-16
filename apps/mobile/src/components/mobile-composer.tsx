import type {
  AgentSession,
  MessageAttachment,
  PendingPermission,
  PendingUserInput,
  UserInputAnswer,
} from '@waku/client';
import * as DocumentPicker from 'expo-document-picker';
import * as Haptics from 'expo-haptics';
import * as ImagePicker from 'expo-image-picker';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import {
  ActivityIndicator,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { AppSymbol } from './app-symbol';
import { AttachmentTile } from './attachment-tile';
import { ComposerAccessMenu } from './composer-access-menu';
import { ComposerContextPicker } from './composer-context-picker';
import { useComposerLocalCommands } from './composer-command-sheets';
import {
  ComposerAddMenu,
  type ComposerAttachmentSource,
} from './composer-add-menu';
import { ComposerTextInput } from './composer-text-input';
import type { ComposerTextInputProps } from './composer-text-input.types';
import { GlassSurface, liquidGlass } from './glass-surface';
import { ModelTraitsSheet } from './session-option-sheets';
import { MonoFont, NativeTint, Radius } from '@/constants/theme';
import { useProviderModels, useTaskState } from '@/hooks/use-daemon-data';
import { useComposerPicker } from '@/hooks/use-composer-picker';
import { useSyncedComposerDraft } from '@/hooks/use-synced-composer-draft';
import { useTheme } from '@/hooks/use-theme';
import {
  importLocalAttachment,
  localFileName,
  type LocalAttachmentFile,
} from '@/lib/attachments';
import { useDaemon } from '@/lib/daemon-context';
import { sessionBusy, sessionCwd } from '@/lib/mobile-runtime';
import { composerProviderPrompt } from '@/lib/composer-completion';
import { modelHasConfigurableTraits } from '@/lib/model-traits';
import { useRuntime } from '@/lib/runtime-context';
import { isDaemonDisconnectError } from '@/lib/runtime-errors';

/**
 * The composer surface shared by the session screen and the new-task screen:
 * a tall rounded card holding the input with an icon toolbar underneath —
 * option toggles on the left, meters and the send button on the right.
 */
export function ComposerCard({
  beforeInput,
  left,
  right,
  ...inputProps
}: ComposerTextInputProps & {
  beforeInput?: ReactNode;
  left?: ReactNode;
  right?: ReactNode;
}) {
  const theme = useTheme();
  return (
    <GlassSurface
      fallbackColor={theme.composer}
      interactive
      style={[
        styles.card,
        !liquidGlass && {
          borderColor: theme.border,
          borderWidth: StyleSheet.hairlineWidth,
        },
      ]}>
      {beforeInput}
      <ComposerTextInput
        multiline
        placeholderTextColor={theme.textTertiary}
        selectionColor={NativeTint}
        style={[styles.input, { color: theme.text }]}
        {...inputProps}
      />
      <View style={styles.toolbar}>
        <View style={styles.cluster}>{left}</View>
        <View style={styles.toolbarSpacer} />
        <View style={styles.cluster}>{right}</View>
      </View>
    </GlassSurface>
  );
}

export function ComposerIconButton({
  icon,
  label,
  onPress,
  active = false,
  disabled = false,
}: {
  icon: Parameters<typeof AppSymbol>[0]['name'];
  label: string;
  onPress: () => void;
  active?: boolean;
  disabled?: boolean;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityLabel={label}
      accessibilityRole="button"
      accessibilityState={{ disabled, selected: active }}
      disabled={disabled}
      hitSlop={4}
      onPress={onPress}
      style={({ pressed }) => [
        styles.iconButton,
        {
          backgroundColor: active ? theme.overlayStrong : 'transparent',
          opacity: disabled ? 0.35 : pressed ? 0.55 : 1,
        },
      ]}>
      <AppSymbol name={icon} size={19} tintColor={active ? NativeTint : theme.textSecondary} />
    </Pressable>
  );
}

export function SendButton({
  onPress,
  disabled,
  busy = false,
  steering = false,
  queueing = false,
  label,
}: {
  onPress: () => void;
  disabled: boolean;
  busy?: boolean;
  steering?: boolean;
  queueing?: boolean;
  label: string;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityLabel={label}
      accessibilityRole="button"
      disabled={disabled}
      hitSlop={4}
      onPress={onPress}
      style={({ pressed }) => [
        styles.sendButton,
        {
          backgroundColor: disabled
            ? theme.surfaceMuted
            : steering ? NativeTint : theme.inverse,
          opacity: pressed || busy ? 0.6 : 1,
        },
      ]}>
      {busy ? (
        <ActivityIndicator color={disabled ? theme.textTertiary : theme.onInverse} size="small" />
      ) : (
        <AppSymbol
          name={queueing
            ? { ios: 'text.append', android: 'playlist_add', web: 'playlist_add' }
            : { ios: 'arrow.up', android: 'arrow_upward', web: 'arrow_upward' }}
          size={17}
          tintColor={disabled ? theme.textTertiary : steering ? '#ffffff' : theme.onInverse}
        />
      )}
    </Pressable>
  );
}

export function MobileComposer({
  session,
  onSubmitted,
}: {
  session: AgentSession;
  /** Fires as the submission begins — before the runtime round-trip — so the
   * transcript can pin to the tail the moment the message lands. */
  onSubmitted?: () => void;
}) {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const [draft, setDraft] = useState('');
  const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
  const [importingAttachments, setImportingAttachments] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const [traitsSheetOpen, setTraitsSheetOpen] = useState(false);
  const modelProbe = useProviderModels(session.provider);
  const models = modelProbe.data?.models ?? [];
  const activeModel = session.model
    ? models.find((item) => item.id === session.model)
    : models.find((item) => item.is_default) ?? models[0];
  const busy = sessionBusy(session);
  const liveRuntime = runtime.runtimes[session.id];
  const canSteer = busy && Boolean(liveRuntime?.supportsSteer) && session.status !== 'connecting';
  const permission = runtime.permissions[session.id];
  const userInput = runtime.userInputs[session.id];
  const runtimeError = runtime.errors[session.id];
  const connected = daemon.phase === 'connected';
  const visibleLocalError = connected && isDaemonDisconnectError(localError) ? null : localError;
  const visibleRuntimeError = connected && isDaemonDisconnectError(runtimeError)
    ? null
    : runtimeError;
  const visibleError = visibleLocalError || visibleRuntimeError;
  const queued = session.queued_messages ?? [];
  const taskState = useTaskState();
  const project = taskState.data?.projects.find((item) => item.id === session.project_id);

  useEffect(() => setLocalError(null), [session.id]);
  useEffect(() => {
    if (connected && isDaemonDisconnectError(localError)) setLocalError(null);
  }, [connected, localError]);

  // Cross-device draft, persisted on the daemon like the desktop composer:
  // hydrate when this surface becomes active, save only local edits, and
  // clear on send. A daemon-loaded value must never be echoed back as an edit.
  const draftSync = useSyncedComposerDraft({
    target: { type: 'session', sessionId: session.id },
    text: draft,
    attachments,
    onHydrate: (synchronized) => {
      setDraft(synchronized.text);
      setAttachments(synchronized.attachments);
    },
    flushOnUnmount: true,
  });
  const contextPicker = useComposerPicker({
    text: draft,
    onChangeText: (value) => {
      draftSync.markEdited();
      setDraft(value);
    },
    provider: session.provider,
    root: project ? sessionCwd(session, project) : null,
    reported: session.available_commands,
    contextKey: session.id,
  });
  const localCommands = useComposerLocalCommands({
    provider: session.provider,
    model: activeModel,
    serviceTier: session.service_tier,
    runtimeMode: session.runtime_mode,
    goal: session.thread_goal,
    contextKey: session.id,
    onServiceTier: (serviceTier) => runtime.updateSessionOptions(session.id, { serviceTier }),
    onGoal: (operation) => runtime.sendGoalOperation(session, operation),
    onClear: () => {
      draftSync.markEdited();
      setDraft('');
    },
  });
  const activeSessionId = useRef(session.id);
  activeSessionId.current = session.id;
  const mounted = useRef(true);
  useEffect(() => () => {
    mounted.current = false;
  }, []);

  const attachmentImportTail = useRef<Promise<void>>(Promise.resolve());
  const pendingAttachmentImports = useRef(0);

  async function addLocalFiles(files: LocalAttachmentFile[]) {
    if (!files.length) return;
    const targetSessionId = session.id;
    pendingAttachmentImports.current += 1;
    setImportingAttachments(true);
    setLocalError(null);
    const operation = attachmentImportTail.current.catch(() => {}).then(async () => {
      const client = daemon.client;
      if (!client || daemon.phase !== 'connected') {
        throw new Error('Waku daemon is disconnected');
      }
      for (const file of files) {
        const imported = await importLocalAttachment(client, file);
        if (mounted.current && activeSessionId.current === targetSessionId) {
          draftSync.markEdited();
          setAttachments((current) => [...current, imported]);
        }
      }
    });
    attachmentImportTail.current = operation;
    try {
      await operation;
      await Haptics.selectionAsync();
    } finally {
      pendingAttachmentImports.current -= 1;
      if (mounted.current && pendingAttachmentImports.current === 0) {
        setImportingAttachments(false);
      }
    }
  }

  async function chooseAttachment(source: ComposerAttachmentSource) {
    try {
      if (source === 'files') {
        const result = await DocumentPicker.getDocumentAsync({
          copyToCacheDirectory: true,
          multiple: true,
          type: '*/*',
        });
        if (!result.canceled) {
          await addLocalFiles(result.assets.map((asset) => ({
            uri: asset.uri,
            name: asset.name,
            mimeType: asset.mimeType,
            size: asset.size,
            base64: asset.base64,
          })));
        }
        return;
      }

      if (source === 'camera') {
        const permission = await ImagePicker.requestCameraPermissionsAsync();
        if (!permission.granted) {
          throw new Error('Camera access is required to take a photo');
        }
        const result = await ImagePicker.launchCameraAsync({
          mediaTypes: ['images'],
          quality: 1,
        });
        if (!result.canceled) await addLocalFiles(imagePickerFiles(result.assets, 'Photo'));
        return;
      }

      const result = await ImagePicker.launchImageLibraryAsync({
        allowsMultipleSelection: true,
        mediaTypes: ['images'],
        quality: 1,
        selectionLimit: 0,
      });
      if (!result.canceled) await addLocalFiles(imagePickerFiles(result.assets, 'Photo'));
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    }
  }

  const requestSignature = permission?.requestId ?? userInput?.requestId;
  const lastRequest = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (requestSignature && requestSignature !== lastRequest.current) {
      void Haptics.notificationAsync(Haptics.NotificationFeedbackType.Warning);
    }
    lastRequest.current = requestSignature;
  }, [requestSignature]);

  async function submit() {
    const prompt = draft.trim();
    const submittedAttachments = attachments;
    if (
      (!prompt && submittedAttachments.length === 0)
      || submitting
      || pendingAttachmentImports.current > 0
    ) return;
    setSubmitting(true);
    setLocalError(null);
    try {
      const commands = prompt.startsWith('/') ? await contextPicker.getCommands() : [];
      if (await localCommands.execute(prompt, commands)) return;
      const providerPrompt = composerProviderPrompt(session.provider, prompt, commands, submittedAttachments);
      onSubmitted?.();
      if (canSteer) await runtime.steerPrompt(session, prompt, submittedAttachments, providerPrompt);
      else await runtime.sendPrompt(session, prompt, submittedAttachments, providerPrompt);
      draftSync.removeSubmittedDraft();
      setDraft('');
      setAttachments([]);
      await Haptics.selectionAsync();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setSubmitting(false);
    }
  }

  async function stop() {
    setLocalError(null);
    try {
      await runtime.cancel(session.id);
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Warning);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  function applyOptions(changes: Parameters<typeof runtime.updateSessionOptions>[1]) {
    runtime.updateSessionOptions(session.id, changes).catch((cause) => {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    });
  }

  const disconnected = daemon.phase !== 'connected';
  const placeholder = disconnected
    ? daemon.phase === 'reconnecting'
      ? 'Reconnecting…'
      : daemon.phase === 'connecting' || daemon.phase === 'booting'
        ? 'Connecting…'
        : 'Reconnect to message this agent'
    : canSteer
      ? 'Message the working agent…'
      : busy
        ? 'Queue a follow-up…'
        : 'Message agent';

  return (
    <View style={[styles.shell, { paddingBottom: Math.max(insets.bottom, 10) }]}>
      {permission && !userInput && (
        <PermissionPanel
          permission={permission}
          onRespond={(optionId) => runtime.respond(session.id, permission.requestId, optionId)}
        />
      )}
      {userInput && (
        <UserInputPanel
          input={userInput}
          onSubmit={(answers) => runtime.respondUserInput(session.id, userInput.requestId, answers)}
        />
      )}
      {visibleError && (
        <View
          accessibilityLiveRegion="polite"
          style={[styles.errorBanner, { backgroundColor: theme.dangerSoft }]}>
          <Text style={[styles.errorText, { color: theme.danger }]}>
            {visibleError}
          </Text>
          <Pressable
            accessibilityLabel="Dismiss error"
            accessibilityRole="button"
            hitSlop={8}
            onPress={() => {
              setLocalError(null);
              runtime.dismissError(session.id);
            }}
            style={({ pressed }) => ({ opacity: pressed ? 0.5 : 1 })}>
            <AppSymbol
              name={{ ios: 'xmark', android: 'close', web: 'close' }}
              size={12}
              tintColor={theme.danger}
            />
          </Pressable>
        </View>
      )}
      {queued.map((message) => (
        <View
          key={message.id}
          style={[styles.queuedRow, { backgroundColor: theme.overlay, borderColor: theme.border }]}>
          <AppSymbol
            name={{ ios: 'clock', android: 'schedule', web: 'schedule' }}
            size={12}
            tintColor={theme.textTertiary}
          />
          <Text numberOfLines={1} style={[styles.queuedText, { color: theme.textSecondary }]}>
            {message.display_content?.trim()
              || message.attachments?.map((attachment) => attachment.name).join(', ')
              || message.content}
          </Text>
          <Pressable
            accessibilityLabel="Remove queued message"
            accessibilityRole="button"
            hitSlop={8}
            onPress={() => void runtime.removeQueuedMessage(session.id, message.id).catch(() => {})}
            style={({ pressed }) => ({ opacity: pressed ? 0.5 : 1 })}>
            <AppSymbol
              name={{ ios: 'xmark', android: 'close', web: 'close' }}
              size={11}
              tintColor={theme.textTertiary}
            />
          </Pressable>
        </View>
      ))}

      <ComposerCard
        {...contextPicker.inputProps}
        accessibilityLabel="Message agent"
        beforeInput={attachments.length || importingAttachments ? (
          <ScrollView
            horizontal
            keyboardShouldPersistTaps="handled"
            showsHorizontalScrollIndicator={false}
            contentContainerStyle={styles.attachmentStrip}>
            {attachments.map((attachment, index) => (
              <AttachmentTile
                attachment={attachment}
                compact
                key={`${attachment.blob_reference ?? attachment.path}:${index}`}
                onRemove={() => {
                  draftSync.markEdited();
                  setAttachments((current) => current.filter((_, item) => item !== index));
                }}
                removeDisabled={submitting}
              />
            ))}
            {importingAttachments && (
              <View style={[styles.attachmentLoading, { backgroundColor: theme.inset, borderColor: theme.border }]}>
                <ActivityIndicator color={theme.textSecondary} size="small" />
                <Text style={[styles.attachmentName, { color: theme.textSecondary }]}>Attaching…</Text>
              </View>
            )}
          </ScrollView>
        ) : undefined}
        editable={!disconnected && !submitting}
        left={(
          <>
            <ComposerAddMenu
              disabled={disconnected || submitting || importingAttachments}
              onChoose={(source) => void chooseAttachment(source)}
              onChooseContext={contextPicker.open}
            />
            <ComposerAccessMenu
              mode={session.runtime_mode}
              onApply={(mode) => applyOptions({ runtimeMode: mode })}
            />
          </>
        )}
        placeholder={placeholder}
        right={(
          <>
            {activeModel && modelHasConfigurableTraits(activeModel) && (
              <ComposerIconButton
                icon={{ ios: 'speedometer', android: 'speed', web: 'speed' }}
                label="Model options"
                onPress={() => setTraitsSheetOpen(true)}
              />
            )}
            {busy && (
              <Pressable
                accessibilityLabel="Stop agent"
                accessibilityRole="button"
                hitSlop={4}
                onPress={() => void stop()}
                style={({ pressed }) => [
                  styles.sendButton,
                  { backgroundColor: theme.dangerSoft, opacity: pressed ? 0.55 : 1 },
                ]}>
                <AppSymbol
                  name={{ ios: 'stop.fill', android: 'stop', web: 'stop' }}
                  size={14}
                  tintColor={theme.danger}
                />
              </Pressable>
            )}
            <SendButton
              busy={submitting}
              disabled={
                (!draft.trim() && attachments.length === 0)
                || submitting
                || importingAttachments
                || disconnected
              }
              label={canSteer ? 'Send to working agent' : busy ? 'Queue message' : 'Send message'}
              onPress={() => void submit()}
              queueing={busy && !canSteer}
              steering={canSteer}
            />
          </>
        )}
        value={draft}
        onPasteError={(message) => {
          setLocalError(message);
          void Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
        }}
        onPasteFiles={(files) => {
          void addLocalFiles(files).catch(async (cause) => {
            setLocalError(cause instanceof Error ? cause.message : String(cause));
            await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
          });
        }}
      />

      {activeModel && (
        <ModelTraitsSheet
          model={activeModel}
          onApply={applyOptions}
          onDismiss={() => setTraitsSheetOpen(false)}
          selection={{
            reasoningEffort: session.reasoning_effort ?? null,
            serviceTier: session.service_tier ?? null,
            contextWindow: session.context_window ?? null,
          }}
          visible={traitsSheetOpen}
        />
      )}
      {localCommands.sheets}
      <ComposerContextPicker {...contextPicker.picker} />
    </View>
  );
}

function imagePickerFiles(
  assets: ImagePicker.ImagePickerAsset[],
  fallbackPrefix: string,
): LocalAttachmentFile[] {
  const timestamp = Date.now();
  return assets.map((asset, index) => ({
    uri: asset.uri,
    name: asset.fileName ?? localFileName(
      asset.uri,
      `${fallbackPrefix}-${timestamp}${assets.length > 1 ? `-${index + 1}` : ''}.jpg`,
    ),
    mimeType: asset.mimeType,
    size: asset.fileSize,
    base64: asset.base64,
  }));
}

function PermissionPanel({
  permission,
  onRespond,
}: {
  permission: PendingPermission;
  onRespond: (optionId: string) => Promise<void>;
}) {
  const theme = useTheme();
  const [responding, setResponding] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <RequestPanel borderColor={theme.warning}>
      <View style={styles.requestHeading}>
        <AppSymbol
          name={{ ios: 'hand.raised.fill', android: 'front_hand', web: 'pan_tool' }}
          size={16}
          tintColor={theme.warning}
        />
        <Text style={[styles.requestTitle, { color: theme.text }]}>{permission.title}</Text>
      </View>
      {permission.detail ? (
        <ScrollView
          nestedScrollEnabled
          style={[styles.detailScroll, { backgroundColor: theme.inset, borderColor: theme.border }]}>
          <Text selectable style={[styles.requestDetail, { color: theme.textSecondary }]}>
            {permission.detail}
          </Text>
        </ScrollView>
      ) : null}
      {error && <Text style={[styles.panelError, { color: theme.danger }]}>{error}</Text>}
      <View style={styles.optionActions}>
        {permission.options.map((option) => (
          <Pressable
            accessibilityRole="button"
            disabled={Boolean(responding)}
            key={option.id}
            onPress={() => {
              setResponding(option.id);
              setError(null);
              void Haptics.selectionAsync();
              void onRespond(option.id).catch((cause) => {
                setError(cause instanceof Error ? cause.message : String(cause));
                setResponding(null);
              });
            }}
            style={({ pressed }) => [
              styles.optionButton,
              {
                backgroundColor: option.allow ? theme.inverse : theme.surfaceMuted,
                opacity: pressed || (responding && responding !== option.id) ? 0.55 : 1,
              },
            ]}>
            {responding === option.id && (
              <ActivityIndicator
                color={option.allow ? theme.onInverse : theme.text}
                size="small"
              />
            )}
            <Text style={[
              styles.optionButtonText,
              { color: option.allow ? theme.onInverse : theme.text },
            ]}>{option.label}</Text>
          </Pressable>
        ))}
      </View>
    </RequestPanel>
  );
}

function UserInputPanel({
  input,
  onSubmit,
}: {
  input: PendingUserInput;
  onSubmit: (answers: UserInputAnswer[]) => Promise<void>;
}) {
  const theme = useTheme();
  const [index, setIndex] = useState(0);
  const [selections, setSelections] = useState<Record<string, string[]>>({});
  const [customAnswers, setCustomAnswers] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setIndex(0);
    setSelections({});
    setCustomAnswers({});
    setSubmitting(false);
    setError(null);
  }, [input.requestId]);

  const question = input.questions[index];
  if (!question) return null;
  const selected = selections[question.id] ?? [];
  const custom = customAnswers[question.id] ?? '';
  const canContinue = Boolean(custom.trim() || selected.length);
  const last = index === input.questions.length - 1;

  function toggle(label: string) {
    void Haptics.selectionAsync();
    setCustomAnswers((values) => ({ ...values, [question.id]: '' }));
    setSelections((values) => {
      const previous = values[question.id] ?? [];
      return {
        ...values,
        [question.id]: question.multiSelect
          ? previous.includes(label)
            ? previous.filter((value) => value !== label)
            : [...previous, label]
          : [label],
      };
    });
  }

  async function advance() {
    if (!canContinue || submitting) return;
    if (!last) {
      setIndex((value) => value + 1);
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await onSubmit(input.questions.map((item) => {
        const customValue = customAnswers[item.id]?.trim();
        return {
          questionId: item.id,
          answers: customValue ? [customValue] : selections[item.id] ?? [],
        };
      }));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setSubmitting(false);
    }
  }

  return (
    <RequestPanel borderColor={theme.accent}>
      <View style={styles.questionHeader}>
        <Text style={[styles.questionEyebrow, { color: theme.textTertiary }]}>{question.header}</Text>
        {input.questions.length > 1 && (
          <Text style={[styles.progress, { color: theme.textTertiary }]}>
            {index + 1} of {input.questions.length}
          </Text>
        )}
      </View>
      <Text style={[styles.questionText, { color: theme.text }]}>{question.question}</Text>
      <ScrollView keyboardShouldPersistTaps="handled" nestedScrollEnabled style={styles.questionOptions}>
        {question.options.map((option) => {
          const checked = selected.includes(option.label);
          return (
            <Pressable
              accessibilityRole={question.multiSelect ? 'checkbox' : 'radio'}
              accessibilityState={{ checked }}
              key={option.label}
              onPress={() => toggle(option.label)}
              style={({ pressed }) => [
                styles.questionOption,
                {
                  backgroundColor: checked ? theme.overlayStrong : theme.surfaceMuted,
                  borderColor: checked ? NativeTint : 'transparent',
                  opacity: pressed ? 0.65 : 1,
                },
              ]}>
              <View style={styles.questionOptionCopy}>
                <Text style={[styles.questionOptionLabel, { color: theme.text }]}>{option.label}</Text>
                {option.description && option.description !== option.label && (
                  <Text style={[styles.questionOptionDescription, { color: theme.textSecondary }]}>
                    {option.description}
                  </Text>
                )}
              </View>
              {checked && (
                <AppSymbol
                  name={{ ios: 'checkmark', android: 'check', web: 'check' }}
                  size={14}
                  tintColor={NativeTint}
                />
              )}
            </Pressable>
          );
        })}
        <TextInput
          accessibilityLabel="Custom answer"
          multiline
          placeholder="Write another answer…"
          placeholderTextColor={theme.textTertiary}
          selectionColor={NativeTint}
          style={[
            styles.customAnswer,
            {
              backgroundColor: theme.surfaceMuted,
              borderColor: custom.trim() ? NativeTint : 'transparent',
              color: theme.text,
            },
          ]}
          value={custom}
          onChangeText={(value) => {
            setCustomAnswers((values) => ({ ...values, [question.id]: value }));
            if (value.trim()) setSelections((values) => ({ ...values, [question.id]: [] }));
          }}
        />
      </ScrollView>
      {error && <Text style={[styles.panelError, { color: theme.danger }]}>{error}</Text>}
      <View style={styles.questionActions}>
        {index > 0 ? (
          <Pressable
            accessibilityRole="button"
            onPress={() => setIndex((value) => value - 1)}
            style={styles.backButton}>
            <Text style={[styles.backButtonText, { color: theme.textSecondary }]}>Back</Text>
          </Pressable>
        ) : <View />}
        <Pressable
          accessibilityRole="button"
          disabled={!canContinue || submitting}
          onPress={() => void advance()}
          style={({ pressed }) => [
            styles.nextButton,
            { backgroundColor: theme.inverse, opacity: !canContinue || submitting || pressed ? 0.55 : 1 },
          ]}>
          {submitting && <ActivityIndicator color={theme.onInverse} size="small" />}
          <Text style={[styles.nextButtonText, { color: theme.onInverse }]}>
            {last ? 'Submit' : 'Next'}
          </Text>
        </Pressable>
      </View>
    </RequestPanel>
  );
}

function RequestPanel({ borderColor, children }: { borderColor: string; children: ReactNode }) {
  const theme = useTheme();
  return (
    <View style={[styles.requestPanel, { backgroundColor: theme.surface, borderColor }]}>
      {children}
    </View>
  );
}

const styles = StyleSheet.create({
  shell: { paddingHorizontal: 12, paddingTop: 4 },
  card: {
    borderRadius: 26,
    paddingBottom: 8,
    paddingHorizontal: 10,
    paddingTop: 6,
  },
  input: {
    fontSize: 16,
    lineHeight: 21,
    maxHeight: 120,
    minHeight: 42,
    paddingHorizontal: 6,
    paddingVertical: 8,
  },
  attachmentStrip: { gap: 8, paddingHorizontal: 4, paddingTop: 4, paddingBottom: 6 },
  attachmentLoading: {
    alignItems: 'center',
    borderRadius: 9,
    borderWidth: 1,
    justifyContent: 'center',
    gap: 7,
    height: 80,
    width: 80,
  },
  attachmentName: { fontSize: 11.5 },
  toolbar: { alignItems: 'center', flexDirection: 'row', marginTop: 2 },
  toolbarSpacer: { flex: 1 },
  cluster: { alignItems: 'center', flexDirection: 'row', gap: 2 },
  iconButton: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    height: 36,
    justifyContent: 'center',
    width: 36,
  },
  sendButton: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    height: 36,
    justifyContent: 'center',
    marginLeft: 4,
    width: 36,
  },
  errorBanner: {
    alignItems: 'center',
    borderRadius: Radius.small,
    flexDirection: 'row',
    gap: 9,
    marginBottom: 7,
    paddingHorizontal: 10,
    paddingVertical: 7,
  },
  errorText: { flex: 1, fontSize: 12, fontWeight: '600', lineHeight: 17 },
  queuedRow: {
    alignItems: 'center',
    borderRadius: Radius.small,
    borderWidth: StyleSheet.hairlineWidth,
    flexDirection: 'row',
    gap: 8,
    marginBottom: 6,
    minHeight: 34,
    paddingHorizontal: 10,
  },
  queuedText: { flex: 1, fontSize: 12.5 },
  requestPanel: {
    borderRadius: Radius.large,
    borderWidth: 1,
    marginBottom: 8,
    padding: 12,
  },
  requestHeading: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  requestTitle: { flex: 1, fontSize: 14, fontWeight: '700' },
  detailScroll: {
    borderRadius: Radius.small,
    borderWidth: StyleSheet.hairlineWidth,
    marginTop: 9,
    maxHeight: 110,
    padding: 9,
  },
  requestDetail: { fontFamily: MonoFont, fontSize: 11.5, lineHeight: 17 },
  optionActions: { flexDirection: 'row', flexWrap: 'wrap', gap: 8, justifyContent: 'flex-end', marginTop: 11 },
  optionButton: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    flexDirection: 'row',
    gap: 6,
    minHeight: 38,
    paddingHorizontal: 14,
  },
  optionButtonText: { fontSize: 13, fontWeight: '700' },
  panelError: { fontSize: 12, lineHeight: 17, marginTop: 8 },
  questionHeader: { alignItems: 'center', flexDirection: 'row', justifyContent: 'space-between' },
  questionEyebrow: { fontSize: 11, fontWeight: '700', letterSpacing: 0.55, textTransform: 'uppercase' },
  progress: { fontSize: 11, fontWeight: '600' },
  questionText: { fontSize: 14, fontWeight: '600', lineHeight: 20, marginTop: 7 },
  questionOptions: { marginTop: 10, maxHeight: 260 },
  questionOption: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    borderWidth: 1,
    flexDirection: 'row',
    gap: 8,
    marginBottom: 6,
    minHeight: 44,
    paddingHorizontal: 11,
    paddingVertical: 8,
  },
  questionOptionCopy: { flex: 1 },
  questionOptionLabel: { fontSize: 13, fontWeight: '600' },
  questionOptionDescription: { fontSize: 11.5, lineHeight: 16, marginTop: 2 },
  customAnswer: {
    borderRadius: Radius.medium,
    borderWidth: 1,
    fontSize: 13,
    lineHeight: 18,
    minHeight: 42,
    paddingHorizontal: 11,
    paddingVertical: 9,
  },
  questionActions: { alignItems: 'center', flexDirection: 'row', justifyContent: 'space-between', marginTop: 10 },
  backButton: { justifyContent: 'center', minHeight: 36, paddingHorizontal: 6 },
  backButtonText: { fontSize: 13, fontWeight: '600' },
  nextButton: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    flexDirection: 'row',
    gap: 6,
    justifyContent: 'center',
    minHeight: 36,
    minWidth: 78,
    paddingHorizontal: 14,
  },
  nextButtonText: { fontSize: 13, fontWeight: '700' },
});
