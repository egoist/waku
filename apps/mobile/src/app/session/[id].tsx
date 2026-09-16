import { useLocalSearchParams } from 'expo-router';

import { SessionView } from '@/components/session-view';

export default function SessionScreen() {
  const params = useLocalSearchParams<{ id?: string | string[]; devPrompt?: string | string[] }>();
  const sessionId = Array.isArray(params.id) ? params.id[0] : params.id;
  const devPrompt = Array.isArray(params.devPrompt) ? params.devPrompt[0] : params.devPrompt;
  return <SessionView devPrompt={__DEV__ ? devPrompt : undefined} sessionId={sessionId} />;
}
