import type { ProviderKind } from '@waku/client';

export interface AgentPresetMenuProps {
  provider: ProviderKind;
  agentPreset: string | null;
  onApply: (selection: { agentPreset: string | null }) => void;
}
