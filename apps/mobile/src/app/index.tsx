import { useState } from 'react';

import { DaemonOnboarding } from '@/components/daemon-onboarding';
import { DaemonPickerSheet } from '@/components/daemon-picker-sheet';
import { useDaemon } from '@/lib/daemon-context';

import NewTaskScreen from './new-task';

export default function HomeScreen() {
  const daemon = useDaemon();
  const [daemonPickerOpen, setDaemonPickerOpen] = useState(false);

  if (daemon.phase === 'booting') return null;

  return (
    <>
      {daemon.profiles.length ? (
        <NewTaskScreen />
      ) : (
        <DaemonOnboarding onAddDaemon={() => setDaemonPickerOpen(true)} />
      )}
      {(!daemon.profiles.length || daemonPickerOpen) && (
        <DaemonPickerSheet
          onDismiss={() => setDaemonPickerOpen(false)}
          visible={daemonPickerOpen}
        />
      )}
    </>
  );
}
