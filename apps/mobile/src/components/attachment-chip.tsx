import type { MessageAttachment } from '@waku/client';
import { useEffect, useState } from 'react';
import { Image, Modal, StyleSheet, Text, View } from 'react-native';

import { AppSymbol } from './app-symbol';
import { AppPressable } from '@/components/app-pressable';

import { Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { isPreviewableImage, readAttachmentImage } from '@/lib/attachments';
import { useDaemon } from '@/lib/daemon-context';

const TILE = 76;

/** One attachment in a message: an inline preview when it is an image the
 * daemon can hand back, otherwise the same name chip as before. */
export function AttachmentChip({ attachment }: { attachment: MessageAttachment }) {
  const theme = useTheme();
  const daemon = useDaemon();
  const [source, setSource] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const { is_dir, is_image, mention, name, path } = attachment;
  const reference = attachment.blob_reference;
  const client = daemon.client;
  const connected = daemon.phase === 'connected';
  const previewable = isPreviewableImage(attachment);

  // Depend on the fields, never the attachment object: every stream commit
  // deep-clones the session, so an object dependency would re-read the blob
  // on each frame.
  useEffect(() => {
    if (!previewable || !reference || !client || !connected) {
      setSource(null);
      return;
    }
    let active = true;
    void readAttachmentImage(client, {
      blob_reference: reference,
      is_dir,
      is_image,
      mention,
      name,
      path,
    })
      .then((value) => { if (active) setSource(value); })
      .catch(() => { if (active) setSource(null); });
    return () => { active = false; };
  }, [client, connected, is_dir, is_image, mention, name, path, previewable, reference]);

  if (source) {
    return (
      <>
        <AppPressable
          accessibilityLabel={`Preview ${attachment.name}`}
          accessibilityRole="button"
          onPress={() => setPreviewing(true)}
          style={({ pressed }) => [styles.tile, { opacity: pressed ? 0.7 : 1 }]}>
          <Image
            resizeMode="cover"
            source={{ uri: source }}
            style={[styles.image, { backgroundColor: theme.inset }]}
          />
        </AppPressable>
        <Modal
          animationType="fade"
          onRequestClose={() => setPreviewing(false)}
          visible={previewing}>
          <View style={[styles.viewer, { backgroundColor: theme.background }]}>
            <Image
              accessibilityLabel={attachment.name}
              resizeMode="contain"
              source={{ uri: source }}
              style={styles.viewerImage}
            />
            <AppPressable
              accessibilityLabel="Close preview"
              accessibilityRole="button"
              hitSlop={8}
              onPress={() => setPreviewing(false)}
              style={[styles.close, { backgroundColor: theme.overlayStrong }]}>
              <AppSymbol
                name={{ ios: 'xmark', android: 'close', web: 'close' }}
                size={15}
                tintColor={theme.text}
              />
            </AppPressable>
          </View>
        </Modal>
      </>
    );
  }

  return (
    <View style={[styles.chip, { backgroundColor: theme.overlayStrong }]}>
      <AppSymbol
        name={{
          ios: is_image ? 'photo' : 'doc',
          android: is_image ? 'image' : 'description',
          web: 'description',
        }}
        size={12}
        tintColor={theme.textSecondary}
      />
      <Text numberOfLines={1} style={[styles.chipText, { color: theme.textSecondary }]}>
        {attachment.name}
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  chip: {
    alignItems: 'center',
    borderRadius: Radius.small,
    flexDirection: 'row',
    gap: 5,
    maxWidth: 180,
    paddingHorizontal: 8,
    paddingVertical: 5,
  },
  chipText: { flexShrink: 1, fontSize: 12 },
  tile: { borderRadius: Radius.small, height: TILE, overflow: 'hidden', width: TILE },
  image: { height: TILE, width: TILE },
  viewer: { alignItems: 'center', flex: 1, justifyContent: 'center' },
  viewerImage: { height: '80%', width: '100%' },
  close: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    height: 32,
    justifyContent: 'center',
    position: 'absolute',
    right: 18,
    top: 54,
    width: 32,
  },
});
