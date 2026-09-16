import { useQuery } from '@tanstack/react-query';
import type { MessageAttachment } from '@waku/client';
import { memo, useCallback, useState } from 'react';
import { ActivityIndicator, Image, Keyboard, Modal, Pressable, StyleSheet, Text, View } from 'react-native';
import { SafeAreaView } from 'react-native-safe-area-context';
import { SvgUri } from 'react-native-svg';

import { AppSymbol } from '@/components/app-symbol';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import { attachmentImageQuery } from '@/lib/attachments';
import { useDaemon } from '@/lib/daemon-context';

/** Fixed-size previews keep the composer and transcript steady while loading.
 * The remove target is a sibling of the preview, so tapping an image is safe. */
export const AttachmentTile = memo(function AttachmentTile({
  attachment,
  compact = false,
  onRemove,
  removeDisabled = false,
}: {
  attachment: MessageAttachment;
  compact?: boolean;
  onRemove?: () => void;
  removeDisabled?: boolean;
}) {
  const theme = useTheme();
  const { activeProfile, client, phase } = useDaemon();
  const options = attachmentImageQuery(client, activeProfile, attachment, phase === 'connected');
  const image = useQuery(options);
  const [failedSource, setFailedSource] = useState<string | null>(null);
  const [preview, setPreview] = useState<{ identity: string; source: string } | null>(null);
  const [focused, setFocused] = useState(false);
  const [removeFocused, setRemoveFocused] = useState(false);
  const identity = JSON.stringify(options.queryKey);
  const source = attachment.is_image && !attachment.is_dir && image.data && image.data !== failedSource
    ? image.data : null;
  const canRetry = image.isError && options.enabled;
  const interactive = Boolean(source || canRetry);
  const onImageError = useCallback(() => setFailedSource(image.data ?? null), [image.data]);

  return (
    <View style={[styles.frame, compact && styles.compact]}>
      <Pressable
        accessibilityLabel={source ? `Preview ${attachment.name}`
          : canRetry ? `Retry preview for ${attachment.name}` : attachment.name}
        accessibilityRole={interactive ? 'button' : 'text'}
        accessibilityState={{ busy: image.isFetching }}
        disabled={!interactive}
        focusable={interactive}
        onBlur={() => setFocused(false)}
        onFocus={() => setFocused(true)}
        onPress={() => {
          if (source) {
            Keyboard.dismiss();
            setPreview({ identity, source });
          } else if (canRetry) void image.refetch();
        }}
        style={({ pressed }) => [
          styles.tile,
          {
            backgroundColor: theme.inset,
            borderColor: focused ? theme.accent : theme.border,
            opacity: pressed ? 0.65 : 1,
          },
        ]}
        tabIndex={interactive ? 0 : -1}>
        {source ? (
          <AttachmentImage source={source} onError={onImageError} />
        ) : (
          <View style={styles.placeholder}>
            {image.isFetching ? <ActivityIndicator color={theme.textTertiary} size="small" /> : (
              <AppSymbol
                name={attachment.is_dir
                  ? { ios: 'folder', android: 'folder', web: 'folder' }
                  : attachment.is_image
                    ? { ios: 'photo', android: 'image', web: 'image' }
                    : { ios: 'doc', android: 'description', web: 'description' }}
                size={18}
                tintColor={theme.textTertiary}
              />
            )}
            <Text numberOfLines={1} style={[styles.name, { color: theme.textSecondary }]}>
              {attachment.name}
            </Text>
            {(canRetry || failedSource === image.data) && (
              <Text style={[styles.status, { color: theme.textTertiary }]}>
                {canRetry ? 'Tap to retry' : 'No preview'}
              </Text>
            )}
          </View>
        )}
      </Pressable>
      {onRemove && (
        <Pressable
          accessibilityLabel={`Remove ${attachment.name}`}
          accessibilityRole="button"
          accessibilityState={{ disabled: removeDisabled }}
          disabled={removeDisabled}
          onBlur={() => setRemoveFocused(false)}
          onFocus={() => setRemoveFocused(true)}
          onPress={onRemove}
          style={({ pressed }) => [styles.removeTarget, { opacity: removeDisabled ? 0.4 : pressed ? 0.65 : 1 }]}
          tabIndex={removeDisabled ? -1 : 0}>
          <View style={[
            styles.removeBadge,
            { backgroundColor: theme.surface, borderColor: removeFocused ? theme.accent : theme.borderStrong },
          ]}>
            <AppSymbol name={{ ios: 'xmark', android: 'close', web: 'close' }} size={11} tintColor={theme.text} />
          </View>
        </Pressable>
      )}
      {preview?.identity === identity && preview.source === source && (
        <AttachmentImagePreview
          name={attachment.name}
          source={preview.source}
          onDismiss={() => setPreview(null)}
          onError={onImageError}
        />
      )}
    </View>
  );
});

function AttachmentImage({
  source,
  contain = false,
  onError,
}: {
  source: string;
  contain?: boolean;
  onError: () => void;
}) {
  if (source.startsWith('data:image/svg+xml;')) {
    return (
      <SvgUri
        height="100%"
        onError={onError}
        preserveAspectRatio={contain ? 'xMidYMid meet' : 'xMidYMid slice'}
        uri={source}
        width="100%"
      />
    );
  }
  return (
    <Image
      accessible={false}
      fadeDuration={0}
      onError={onError}
      resizeMethod="resize"
      resizeMode={contain ? 'contain' : 'cover'}
      source={{ uri: source }}
      style={StyleSheet.absoluteFill}
    />
  );
}

function AttachmentImagePreview({
  name,
  source,
  onDismiss,
  onError,
}: {
  name: string;
  source: string;
  onDismiss: () => void;
  onError: () => void;
}) {
  const theme = useTheme();
  const reducedMotion = useReducedMotion();
  const [focused, setFocused] = useState(false);
  return (
    <Modal
      animationType={reducedMotion ? 'none' : 'fade'}
      onRequestClose={onDismiss}
      presentationStyle="fullScreen"
      supportedOrientations={['portrait', 'landscape']}
      visible>
      <SafeAreaView
        accessibilityViewIsModal
        onAccessibilityEscape={onDismiss}
        style={[styles.preview, { backgroundColor: theme.background }]}>
        <View style={styles.previewHeader}>
          <Text numberOfLines={1} style={[styles.previewTitle, { color: theme.text }]}>{name}</Text>
          <Pressable
            accessibilityLabel="Close image preview"
            accessibilityRole="button"
            onBlur={() => setFocused(false)}
            onFocus={() => setFocused(true)}
            onPress={onDismiss}
            style={({ pressed }) => [styles.close, {
              backgroundColor: theme.raised,
              borderColor: focused ? theme.accent : 'transparent',
              opacity: pressed ? 0.65 : 1,
            }]}
            tabIndex={0}>
            <AppSymbol name={{ ios: 'xmark', android: 'close', web: 'close' }} size={17} tintColor={theme.text} />
          </Pressable>
        </View>
        <View accessibilityLabel={name} accessibilityRole="image" accessible style={styles.previewImage}>
          <AttachmentImage contain source={source} onError={onError} />
        </View>
      </SafeAreaView>
    </Modal>
  );
}

const styles = StyleSheet.create({
  frame: { width: 96, height: 80, flexShrink: 0 },
  compact: { width: 80 },
  tile: { flex: 1, borderRadius: 9, borderWidth: 1, overflow: 'hidden' },
  placeholder: { flex: 1, alignItems: 'center', justifyContent: 'center', gap: 7, paddingHorizontal: 7 },
  name: { alignSelf: 'stretch', fontSize: 11.5, textAlign: 'center' },
  status: { fontSize: 10, marginTop: -4 },
  removeTarget: { position: 'absolute', top: 0, right: 0, width: 44, height: 44, alignItems: 'flex-end', padding: 3 },
  removeBadge: { width: 26, height: 26, borderRadius: 13, borderWidth: 1, alignItems: 'center', justifyContent: 'center' },
  preview: { flex: 1 },
  previewHeader: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, paddingVertical: 8 },
  previewTitle: { flex: 1, fontSize: 15, fontWeight: '600' },
  close: { width: 44, height: 44, borderRadius: 22, borderWidth: 1, alignItems: 'center', justifyContent: 'center' },
  previewImage: { flex: 1, margin: 16 },
});
