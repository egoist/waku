export * from '@waku/client/transcript-presentation'

import type { AgentSession } from '@waku/client'
import {
  providerSupportsConversationFork,
  type AssistantResponseFooter,
} from '@waku/client/transcript-presentation'

export function responseForkTurnCount(
  session: AgentSession,
  message: AgentSession['messages'][number],
  footer: AssistantResponseFooter | null,
  turn?: AgentSession['turns'][number],
): number | null {
  if (
    !footer
    || message.role !== 'assistant'
    || !['idle', 'failed'].includes(session.status)
    || !providerSupportsConversationFork(session.provider)
    || !session.provider_cursor
    || session.provider_cursor.provider !== session.provider
    || !turn?.provider_turn_started
  ) return null
  return turn.turn_count
}
