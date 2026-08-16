import type {
  ActivityItem,
  AgentSession,
  MessageAttachment,
  ReviewDiffSource,
} from '@waku/client'
import { ContextMenu } from '@base-ui/react/context-menu'
import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode, type RefObject } from 'react'
import { Virtuoso, type ListItem, type VirtuosoHandle } from 'react-virtuoso'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { PreviewableImage } from '@/components/image-preview'
import { FileTypeIcon, WakuIcon, type WakuIconName } from '@/components/waku-icon'
import { readAttachmentImage } from '@/lib/attachments'
import { useDaemon } from '@/lib/daemon-context'
import { activitiesForBlock } from '@/lib/event-reducer'
import { useI18n, type AppLocale } from '@/lib/i18n'
import {
  advanceMarkdownVeil,
  createMarkdownVeilState,
  markdownVeilPlugin,
} from '@/lib/markdown-veil'
import type {
  BackgroundWorkItem,
  BackgroundWorkKey,
  BackgroundWorkStatus,
} from '@/lib/runtime-context'
import {
  activityActionLabel,
  activityDisclosureSections,
  activityDisplayTitle,
  activityFileChangeStats,
  activityHeaderTitle,
  activityPreview,
  activityRowDetail,
  activityTextRows,
  assistantResponseFooters,
  fencedCode,
  formatDuration,
  formatMessageTime,
  formatWorkingElapsed,
  reasoningTitle,
  shouldVirtualizeActivityText,
  turnAnswerStart,
  turnFoldLabel,
  userMessageRewindTurnCount,
} from '@/lib/transcript-presentation'
import type { AssistantResponseFooter, Translator } from '@/lib/transcript-presentation'
import {
  activeNavigationTurn,
  firstVisibleTranscriptItem,
} from '@/lib/transcript-navigation'
import { cn } from '@/lib/utils'

const NAVIGATION_RAIL_MIN_WIDTH = 872
const NAVIGATION_RAIL_PITCH = 12
const NAVIGATION_PREVIEW_HEIGHT = 126
const TranscriptLinkContext = createContext<(target: string) => boolean>(() => false)

type NavigationTurn = {
  messageId: string
  prompt: string
  response: string
}

type ResponseForkAction = {
  turnCount: number
  pending: boolean
  onFork: (turnCount: number) => void
}

type MessageRewindAction = {
  turnCount: number
  pending: boolean
  onBegin: () => void
}

type MessageEdit = {
  messageId: string
  turnCount: number
  content: string
  attachments: MessageAttachment[]
}

export function Transcript({
  session,
  backgroundWork = [],
  onReviewChanges,
  onOpenLink,
  onOpenBackgroundWork,
  onCopyToComposer,
  onForkResponse,
  onRewindMessage,
  forkingTurnCount,
  rewindTurnCounts = [],
  rewindingTurnCount,
}: {
  session: AgentSession
  backgroundWork?: BackgroundWorkItem[]
  onReviewChanges?: (source: ReviewDiffSource) => void
  onOpenLink?: (target: string) => boolean
  onOpenBackgroundWork?: (key: BackgroundWorkKey) => void
  onCopyToComposer?: (content: string) => void
  onForkResponse?: (turnCount: number) => void
  onRewindMessage?: (
    turnCount: number,
    prompt: string,
    attachments: MessageAttachment[],
  ) => Promise<void>
  forkingTurnCount?: number
  rewindTurnCounts?: readonly number[]
  rewindingTurnCount?: number
}) {
  const { locale, t } = useI18n()
  const root = useRef<HTMLDivElement>(null)
  const transcript = useRef<VirtuosoHandle>(null)
  const transcriptScroller = useRef<HTMLElement | null>(null)
  const renderedItems = useRef<ListItem<TranscriptRenderItem>[]>([])
  const renderedItemsSession = useRef(session.id)
  const navigationTurnsRef = useRef<Array<NavigationTurn & { itemIndex: number }>>([])
  const activeTurnFrame = useRef<number | null>(null)
  const wasNearBottom = useRef(true)
  const expandedSession = useRef(session.id)
  const [expandedTurns, setExpandedTurns] = useState<Set<string>>(new Set())
  const [atTop, setAtTop] = useState(true)
  const [atBottom, setAtBottom] = useState(true)
  const [railFits, setRailFits] = useState(false)
  const [activeTurnIndex, setActiveTurnIndex] = useState(() => Math.max(0, session.turns.length - 1))
  const [messageEdit, setMessageEdit] = useState<MessageEdit | null>(null)
  const items = buildTranscriptItems(session, expandedTurns, new Set(rewindTurnCounts))
  const itemIndexesByMessage = new Map<string, number>()
  items.forEach((item, index) => {
    if (item.kind === 'message' && item.message.role === 'user') {
      itemIndexesByMessage.set(item.message.id, index)
    }
  })
  const navigationTurns = transcriptNavigationTurns(session).flatMap((turn) => {
    const itemIndex = itemIndexesByMessage.get(turn.messageId)
    return itemIndex === undefined ? [] : [{ ...turn, itemIndex }]
  })
  navigationTurnsRef.current = navigationTurns
  if (renderedItemsSession.current !== session.id) {
    renderedItemsSession.current = session.id
    renderedItems.current = []
  }

  const updateActiveTurnFromViewport = useCallback(() => {
    activeTurnFrame.current = null
    const scroller = transcriptScroller.current
    if (!scroller) return
    const atBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 100
    const firstVisibleItem = firstVisibleTranscriptItem(renderedItems.current, scroller.scrollTop)
    if (firstVisibleItem === null && !atBottom) return
    const active = activeNavigationTurn(
      navigationTurnsRef.current,
      firstVisibleItem ?? 0,
      atBottom,
    )
    if (active !== null) setActiveTurnIndex(active)
  }, [])

  const scheduleActiveTurnUpdate = useCallback(() => {
    if (activeTurnFrame.current !== null) return
    activeTurnFrame.current = requestAnimationFrame(updateActiveTurnFromViewport)
  }, [updateActiveTurnFromViewport])

  const setTranscriptScroller = useCallback((next: HTMLElement | Window | null) => {
    const element = next instanceof HTMLElement ? next : null
    if (transcriptScroller.current === element) return
    transcriptScroller.current?.removeEventListener('scroll', scheduleActiveTurnUpdate)
    transcriptScroller.current = element
    transcriptScroller.current?.addEventListener('scroll', scheduleActiveTurnUpdate, { passive: true })
    scheduleActiveTurnUpdate()
  }, [scheduleActiveTurnUpdate])

  useEffect(() => () => {
    transcriptScroller.current?.removeEventListener('scroll', scheduleActiveTurnUpdate)
    if (activeTurnFrame.current !== null) cancelAnimationFrame(activeTurnFrame.current)
  }, [scheduleActiveTurnUpdate])

  useEffect(() => {
    if (expandedSession.current === session.id) return
    expandedSession.current = session.id
    setExpandedTurns(new Set())
    setMessageEdit(null)
    wasNearBottom.current = true
    setAtBottom(true)
    setActiveTurnIndex(Math.max(0, navigationTurns.length - 1))
  }, [session.id])

  useEffect(() => {
    const rootElement = root.current
    if (!rootElement) return
    const resizeObserver = new ResizeObserver(() => {
      setRailFits(rootElement.clientWidth >= NAVIGATION_RAIL_MIN_WIDTH)
    })
    resizeObserver.observe(rootElement)
    setRailFits(rootElement.clientWidth >= NAVIGATION_RAIL_MIN_WIDTH)
    return () => resizeObserver.disconnect()
  }, [])

  const empty = session.messages.length === 0 && session.transcript_blocks.length === 0
  return (
    <TranscriptLinkContext.Provider value={onOpenLink ?? (() => false)}>
      <div className="relative min-h-0 flex-1" ref={root}>
      {empty ? (
        <div className="absolute inset-0 grid place-items-center pb-8">
          <div className="text-center">
            <WakuIcon className="mx-auto size-5 text-ring" name="sparkle" />
            <h2 className="mt-3 text-xl font-medium tracking-tight">
              {t('onboarding.what_should_we_build')}
            </h2>
          </div>
        </div>
      ) : (
        <Virtuoso
          atBottomStateChange={(nextAtBottom) => {
            wasNearBottom.current = nextAtBottom
            setAtBottom(nextAtBottom)
            if (nextAtBottom && navigationTurns.length) {
              setActiveTurnIndex(navigationTurns.length - 1)
            }
          }}
          atBottomThreshold={100}
          atTopStateChange={setAtTop}
          className="absolute inset-0 overscroll-contain"
          computeItemKey={(_, item) => item.key}
          data={items}
          defaultItemHeight={64}
          followOutput={(isAtBottom) => isAtBottom || wasNearBottom.current ? 'auto' : false}
          increaseViewportBy={{ top: 800, bottom: 1_200 }}
          initialTopMostItemIndex={{ index: Math.max(0, items.length - 1), align: 'end' }}
          itemsRendered={(nextItems) => {
            renderedItems.current = nextItems
            scheduleActiveTurnUpdate()
          }}
          itemContent={(_, item) => (
            <TranscriptItemView
              backgroundWork={backgroundWork}
              item={item}
              locale={locale}
              sessionId={session.id}
              t={t}
              onCopyToComposer={onCopyToComposer}
              onOpenBackgroundWork={onOpenBackgroundWork}
              onForkResponse={onForkResponse}
              forkingTurnCount={forkingTurnCount}
              messageEdit={messageEdit}
              onBeginMessageEdit={(message, turnCount) => setMessageEdit({
                messageId: message.id,
                turnCount,
                content: message.display_content ?? message.content,
                attachments: message.attachments ?? [],
              })}
              onCancelMessageEdit={() => {
                if (messageEdit?.turnCount !== rewindingTurnCount) setMessageEdit(null)
              }}
              onSubmitMessageEdit={async (prompt) => {
                if (!messageEdit || !onRewindMessage) return
                const editing = messageEdit
                try {
                  await onRewindMessage(
                    editing.turnCount,
                    prompt,
                    editing.attachments,
                  )
                  setMessageEdit((current) => current?.messageId === editing.messageId ? null : current)
                } catch {
                  // The app-level handler reports the failure. Keep the edit
                  // open so the user's replacement remains recoverable.
                }
              }}
              rewindingTurnCount={rewindingTurnCount}
              onReviewChanges={onReviewChanges}
              onToggleTurn={(turnId) => setExpandedTurns((current) => {
                const next = new Set(current)
                if (next.has(turnId)) next.delete(turnId)
                else next.add(turnId)
                return next
              })}
            />
          )}
          key={session.id}
          minOverscanItemCount={{ top: 2, bottom: 3 }}
          ref={transcript}
          scrollerRef={setTranscriptScroller}
          totalListHeightChanged={() => {
            if (wasNearBottom.current) {
              transcript.current?.scrollToIndex({
                index: Math.max(0, items.length - 1),
                align: 'end',
                behavior: 'auto',
              })
            }
          }}
        />
      )}
      {railFits && !(atTop && atBottom) && navigationTurns.length >= 2 && (
        <ConversationNavigationRail
          activeIndex={Math.min(activeTurnIndex, navigationTurns.length - 1)}
          t={t}
          turns={navigationTurns}
          onActivate={(index) => {
            wasNearBottom.current = false
            setActiveTurnIndex(index)
            transcript.current?.scrollToIndex({
              index: navigationTurns[index]!.itemIndex,
              align: 'start',
              behavior: 'auto',
            })
          }}
        />
      )}
      {!atBottom && !empty && (
        <button
          aria-label={t('transcript.scroll_to_bottom')}
          className="absolute bottom-2 left-1/2 z-10 grid size-8 -translate-x-1/2 place-items-center rounded-full border bg-card shadow-sm outline-none hover:bg-[var(--raised)] focus-visible:ring-1 focus-visible:ring-ring"
          type="button"
          onClick={() => transcript.current?.scrollToIndex({
            index: Math.max(0, items.length - 1),
            align: 'end',
            behavior: 'auto',
          })}
        >
          <WakuIcon className="size-4" name="arrowDown" />
        </button>
      )}
      </div>
    </TranscriptLinkContext.Provider>
  )
}

function transcriptNavigationTurns(session: AgentSession): NavigationTurn[] {
  const userIndexes: number[] = []
  session.messages.forEach((message, index) => {
    if (message.role === 'user') userIndexes.push(index)
  })
  const turnsById = new Map(session.turns.map((turn) => [turn.id, turn]))
  return userIndexes.map((messageIndex, turnIndex) => {
    const message = session.messages[messageIndex]!
    const nextUserIndex = userIndexes[turnIndex + 1] ?? session.messages.length
    const visible = message.display_content ?? message.content
    const prompt = visible.trim()
      ? navigationPreviewSnippet(visible, 100)
      : (message.attachments ?? []).map((attachment) => attachment.name).join(', ')
    const running = message.turn_id
      ? turnsById.get(message.turn_id)?.status === 'running'
      : false
    let response = ''
    if (!running) {
      for (let index = nextUserIndex - 1; index > messageIndex; index--) {
        const candidate = session.messages[index]!
        if (candidate.role === 'assistant' && candidate.content.trim()) {
          response = navigationPreviewSnippet(candidate.content, 240)
          break
        }
      }
    }
    return { messageId: message.id, prompt, response }
  })
}

function navigationPreviewSnippet(content: string, limit: number) {
  const normalized = content.trim().split(/\s+/).join(' ')
  const characters = Array.from(normalized)
  return characters.length > limit ? `${characters.slice(0, limit).join('')}…` : normalized
}

function ConversationNavigationRail({
  turns,
  activeIndex,
  t,
  onActivate,
}: {
  turns: NavigationTurn[]
  activeIndex: number
  t: Translator
  onActivate: (index: number) => void
}) {
  const root = useRef<HTMLElement>(null)
  const rail = useRef<VirtuosoHandle>(null)
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)
  const [focusedIndex, setFocusedIndex] = useState<number | null>(null)
  const [previewTop, setPreviewTop] = useState(0)
  const [atTop, setAtTop] = useState(true)
  const [atBottom, setAtBottom] = useState(true)
  const totalHeight = turns.length * NAVIGATION_RAIL_PITCH
  const emphasizedIndex = hoveredIndex ?? focusedIndex

  function updatePreviewPosition(index: number) {
    const railBounds = root.current?.getBoundingClientRect()
    const tickBounds = document
      .getElementById(`conversation-navigation-turn-${turns[index]!.messageId}`)
      ?.getBoundingClientRect()
    if (!railBounds || !tickBounds) return
    const centered = tickBounds.top - railBounds.top
      + tickBounds.height / 2 - NAVIGATION_PREVIEW_HEIGHT / 2
    setPreviewTop(Math.max(0, Math.min(railBounds.height - NAVIGATION_PREVIEW_HEIGHT, centered)))
  }

  function focusTurn(index: number) {
    rail.current?.scrollIntoView({ index, behavior: 'auto' })
    requestAnimationFrame(() => {
      document.getElementById(`conversation-navigation-turn-${turns[index]!.messageId}`)?.focus()
    })
  }

  useEffect(() => {
    rail.current?.scrollIntoView({ index: activeIndex, behavior: 'auto' })
  }, [activeIndex, turns.length])

  return (
    <nav
      aria-label={t('transcript.conversation_turns')}
      className="absolute left-4 top-1/2 z-10 w-11 -translate-y-1/2"
      ref={root}
      style={{ height: `min(${totalHeight}px, 80%)` }}
    >
      <Virtuoso
        atBottomStateChange={setAtBottom}
        atTopStateChange={setAtTop}
        className="h-full w-11 overscroll-contain"
        computeItemKey={(_, turn) => turn.messageId}
        data={turns}
        fixedItemHeight={NAVIGATION_RAIL_PITCH}
        increaseViewportBy={NAVIGATION_RAIL_PITCH * 5}
        itemContent={(index, turn) => {
          const emphasizedDistance = emphasizedIndex === null
            ? Number.POSITIVE_INFINITY
            : Math.abs(index - emphasizedIndex)
          const scale = emphasizedDistance === 0
            ? 1
            : emphasizedDistance === 1
              ? 0.68
              : emphasizedDistance === 2
                ? 0.44
                : 0.25
          const prominent = index === activeIndex || index === emphasizedIndex
          return (
            <button
              aria-current={index === activeIndex ? 'step' : undefined}
              aria-label={t('transcript.turn_label', { number: index + 1, prompt: turn.prompt })}
              className="flex h-3 w-11 items-center rounded outline-none focus-visible:ring-1 focus-visible:ring-ring"
              id={`conversation-navigation-turn-${turn.messageId}`}
              tabIndex={index === activeIndex ? 0 : -1}
              type="button"
              onBlur={() => setFocusedIndex((current) => current === index ? null : current)}
              onClick={(event) => {
                setHoveredIndex(null)
                setFocusedIndex(null)
                if (event.detail > 0) event.currentTarget.blur()
                onActivate(index)
              }}
              onFocus={() => {
                setFocusedIndex(index)
                requestAnimationFrame(() => updatePreviewPosition(index))
              }}
              onKeyDown={(event) => {
                let target: number | null = null
                if (event.key === 'ArrowUp') target = Math.max(0, index - 1)
                else if (event.key === 'ArrowDown') target = Math.min(turns.length - 1, index + 1)
                else if (event.key === 'Home') target = 0
                else if (event.key === 'End') target = turns.length - 1
                if (target === null) return
                event.preventDefault()
                focusTurn(target)
              }}
              onMouseEnter={() => {
                setHoveredIndex(index)
                requestAnimationFrame(() => updatePreviewPosition(index))
              }}
              onMouseLeave={() => setHoveredIndex((current) => current === index ? null : current)}
            >
              <span
                className={cn(
                  'h-0.5 rounded-full transition-[width] duration-300 ease-out motion-reduce:transition-none',
                  prominent ? 'bg-foreground' : 'bg-[var(--text-ghost)] opacity-45',
                )}
                style={{ width: 32 * scale }}
              />
            </button>
          )
        }}
        rangeChanged={() => {
          if (emphasizedIndex !== null) {
            requestAnimationFrame(() => updatePreviewPosition(emphasizedIndex))
          }
        }}
        ref={rail}
      />
      <div
        aria-hidden="true"
        className={cn(
          'pointer-events-none absolute inset-x-0 top-0 h-5 bg-gradient-to-b from-background to-transparent transition-opacity',
          'motion-reduce:transition-none',
          !atTop ? 'opacity-100' : 'opacity-0',
        )}
      />
      <div
        aria-hidden="true"
        className={cn(
          'pointer-events-none absolute inset-x-0 bottom-0 h-5 bg-gradient-to-t from-background to-transparent transition-opacity',
          'motion-reduce:transition-none',
          !atBottom ? 'opacity-100' : 'opacity-0',
        )}
      />
      {emphasizedIndex !== null && (
        <div
          className="waku-popover-surface pointer-events-none absolute left-[60px] z-20 flex max-h-[126px] w-80 flex-col gap-1.5 overflow-hidden rounded-[14px] px-[15px] py-3 text-popover-foreground"
          style={{ top: previewTop }}
        >
          <div className="truncate text-sm font-semibold leading-5">
            {turns[emphasizedIndex]!.prompt}
          </div>
          {turns[emphasizedIndex]!.response && (
            <div className="max-h-[60px] overflow-hidden text-[13px] leading-5 text-[var(--text-tertiary)]">
              {turns[emphasizedIndex]!.response}
            </div>
          )}
        </div>
      )}
    </nav>
  )
}

type TranscriptRenderItem =
  | {
      kind: 'fold'
      key: string
      turnId: string
      turn: AgentSession['turns'][number]
      expanded: boolean
    }
  | {
      kind: 'block'
      key: string
      block: AgentSession['transcript_blocks'][number]
      liveTurn: boolean
    }
  | {
      kind: 'message'
      key: string
      message: AgentSession['messages'][number]
      first: boolean
      followUp: boolean
      footer: AssistantResponseFooter | null
      forkTurnCount: number | null
      rewindTurnCount: number | null
      checkpoint: AgentSession['turns'][number]['checkpoint'] | null
    }
  | {
      kind: 'changed'
      key: string
      turnId: string
      checkpoint: NonNullable<AgentSession['turns'][number]['checkpoint']>
    }
  | {
      kind: 'working'
      key: 'working'
      startedAt: number
    }
  | {
      kind: 'tail'
      key: 'tail'
    }

function buildTranscriptItems(
  session: AgentSession,
  expandedTurns: Set<string>,
  availableRewindTurnCounts: ReadonlySet<number>,
): TranscriptRenderItem[] {
  const turns = new Map(session.turns.map((turn) => [turn.id, turn]))
  const rawRows = transcriptRows(session)
  const folds = turnFolds(session, rawRows)
  const responseFooters = assistantResponseFooters(session)
  const inlineCheckpoints = new Map<number, NonNullable<AgentSession['turns'][number]['checkpoint']>>()
  const standaloneCheckpoints = new Map<string, NonNullable<AgentSession['turns'][number]['checkpoint']>>()
  const lastAssistantIndexByTurn = new Map<string, number>()
  session.messages.forEach((message, index) => {
    if (message.role === 'assistant' && message.turn_id) {
      lastAssistantIndexByTurn.set(message.turn_id, index)
    }
  })
  for (const turn of session.turns) {
    if (turn.status === 'running' || turn.checkpoint?.status !== 'ready' || !turn.checkpoint.files.length) {
      continue
    }
    const messageIndex = lastAssistantIndexByTurn.get(turn.id)
    if (messageIndex !== undefined && session.messages[messageIndex]!.content.trim()) {
      inlineCheckpoints.set(messageIndex, turn.checkpoint)
    } else {
      standaloneCheckpoints.set(turn.id, turn.checkpoint)
    }
  }
  const items: TranscriptRenderItem[] = []
  let seenUserMessage = false
  for (const row of rawRows) {
    const fold = folds.anchors.get(row.key)
    if (fold) {
      items.push({
        kind: 'fold',
        key: `fold-${fold.turnId}`,
        turnId: fold.turnId,
        turn: fold.turn,
        expanded: expandedTurns.has(fold.turnId),
      })
    }
    if (folds.hidden.has(row.key) && !expandedTurns.has(row.turnId ?? '')) continue
    if (row.kind === 'block') {
      items.push({
        kind: 'block',
        key: row.key,
        block: row.block,
        liveTurn: Boolean(
          row.block.turn_id
          && turns.get(row.block.turn_id)?.status === 'running',
        ),
      })
      continue
    }
    const { message, index } = row
    const startsFollowUp = message.role === 'user' && seenUserMessage
    if (message.role === 'user') seenUserMessage = true
    const footer = responseFooters.get(index) ?? null
    const turn = message.turn_id ? turns.get(message.turn_id) : undefined
    items.push({
      kind: 'message',
      key: row.key,
      message,
      first: index === 0,
      followUp: startsFollowUp,
      footer,
      forkTurnCount: responseForkTurnCount(session, message, footer, turn),
      rewindTurnCount: userMessageRewindTurnCount(
        session,
        message,
        availableRewindTurnCounts,
      ),
      checkpoint: inlineCheckpoints.get(index) ?? null,
    })
  }

  if (standaloneCheckpoints.size) {
    const lastItemByTurn = new Map<string, number>()
    items.forEach((item, index) => {
      const turnId = item.kind === 'fold' || item.kind === 'changed'
        ? item.turnId
        : item.kind === 'block'
          ? item.block.turn_id
          : item.kind === 'message'
            ? item.message.turn_id
            : null
      if (turnId && standaloneCheckpoints.has(turnId)) lastItemByTurn.set(turnId, index)
    })
    const changedAfter = new Map<number, Array<{ turnId: string; checkpoint: NonNullable<AgentSession['turns'][number]['checkpoint']> }>>()
    for (const [turnId, checkpoint] of standaloneCheckpoints) {
      const index = lastItemByTurn.get(turnId)
      if (index === undefined) continue
      const entries = changedAfter.get(index) ?? []
      entries.push({ turnId, checkpoint })
      changedAfter.set(index, entries)
    }
    if (changedAfter.size) {
      const withChanges: TranscriptRenderItem[] = []
      items.forEach((item, index) => {
        withChanges.push(item)
        for (const entry of changedAfter.get(index) ?? []) {
          withChanges.push({
            kind: 'changed',
            key: `changed-${entry.turnId}`,
            turnId: entry.turnId,
            checkpoint: entry.checkpoint,
          })
        }
      })
      items.splice(0, items.length, ...withChanges)
    }
  }

  if (['connecting', 'working', 'waiting'].includes(session.status)) {
    const runningIndex = lastIndexWhere(session.turns, (turn) => turn.status === 'running')
    const running = runningIndex >= 0 ? session.turns[runningIndex] : null
    if (running) {
    items.push({
      kind: 'working',
      key: 'working',
        startedAt: running.started_at,
    })
    }
  }
  items.push({ kind: 'tail', key: 'tail' })
  return items
}

function responseForkTurnCount(
  session: AgentSession,
  message: AgentSession['messages'][number],
  footer: AssistantResponseFooter | null,
  turn?: AgentSession['turns'][number],
): number | null {
  if (
    !footer
    || message.role !== 'assistant'
    || !['idle', 'failed'].includes(session.status)
    || !session.provider_cursor
    || session.provider_cursor.provider !== session.provider
    || !turn?.provider_turn_started
  ) return null
  return turn.turn_count
}

function TranscriptItemView({
  item,
  backgroundWork,
  locale,
  sessionId,
  t,
  onToggleTurn,
  onReviewChanges,
  onCopyToComposer,
  onOpenBackgroundWork,
  onForkResponse,
  forkingTurnCount,
  messageEdit,
  onBeginMessageEdit,
  onCancelMessageEdit,
  onSubmitMessageEdit,
  rewindingTurnCount,
}: {
  item: TranscriptRenderItem
  backgroundWork: BackgroundWorkItem[]
  locale: AppLocale
  sessionId: string
  t: Translator
  onToggleTurn: (turnId: string) => void
  onReviewChanges?: (source: ReviewDiffSource) => void
  onCopyToComposer?: (content: string) => void
  onOpenBackgroundWork?: (key: BackgroundWorkKey) => void
  onForkResponse?: (turnCount: number) => void
  forkingTurnCount?: number
  messageEdit: MessageEdit | null
  onBeginMessageEdit: (
    message: AgentSession['messages'][number],
    turnCount: number,
  ) => void
  onCancelMessageEdit: () => void
  onSubmitMessageEdit: (prompt: string) => Promise<void>
  rewindingTurnCount?: number
}) {
  if (item.kind === 'tail') return <div className="h-5" />
  let content: ReactNode
  if (item.kind === 'fold') {
    content = (
      <TurnFold
        expanded={item.expanded}
        label={turnFoldLabel(item.turn, t)}
        onToggle={() => onToggleTurn(item.turnId)}
      />
    )
  } else if (item.kind === 'block') {
    content = (
      <ActivityGroup
        activities={activitiesForBlock(item.block)}
        backgroundWork={backgroundWork}
        liveTurn={item.liveTurn}
        t={t}
        onOpenBackgroundWork={onOpenBackgroundWork}
      />
    )
  } else if (item.kind === 'changed') {
    content = (
      <ChangedFilesCard
        additions={item.checkpoint.additions}
        deletions={item.checkpoint.deletions}
        files={item.checkpoint.files}
        t={t}
        onReview={onReviewChanges
          ? () => onReviewChanges({
              lastTurn: {
                session_id: sessionId,
                turn_id: item.turnId,
                turn_count: item.checkpoint.turn_count,
              },
            })
          : undefined}
      />
    )
  } else if (item.kind === 'working') {
    content = <WorkingIndicator startedAt={item.startedAt} t={t} />
  } else {
    content = (
      <MessageRow
        footer={item.footer}
        locale={locale}
        message={item.message}
        t={t}
        onCopyToComposer={onCopyToComposer}
        editing={messageEdit?.messageId === item.message.id}
        editContent={messageEdit?.messageId === item.message.id ? messageEdit.content : undefined}
        onCancelEdit={onCancelMessageEdit}
        onSubmitEdit={onSubmitMessageEdit}
        forkAction={item.forkTurnCount && onForkResponse
          ? {
              turnCount: item.forkTurnCount,
              pending: forkingTurnCount === item.forkTurnCount,
              onFork: onForkResponse,
            }
          : undefined}
        rewindAction={item.rewindTurnCount
          ? {
              turnCount: item.rewindTurnCount,
              pending: rewindingTurnCount === item.rewindTurnCount,
              onBegin: () => onBeginMessageEdit(item.message, item.rewindTurnCount!),
            }
          : undefined}
        beforeFooter={item.checkpoint?.status === 'ready' && item.checkpoint.files.length > 0
          ? (
            <ChangedFilesCard
              additions={item.checkpoint.additions}
              deletions={item.checkpoint.deletions}
              files={item.checkpoint.files}
              t={t}
              onReview={onReviewChanges && item.message.turn_id
                ? () => onReviewChanges({
                    lastTurn: {
                      session_id: sessionId,
                      turn_id: item.message.turn_id!,
                      turn_count: item.checkpoint!.turn_count,
                    },
                  })
                : undefined}
            />
          )
          : null}
      />
    )
  }
  return (
    <div className="mx-auto w-full max-w-[760px] px-5 sm:px-5">
      <TranscriptRow
        first={item.kind === 'message' && item.first}
        followUp={item.kind === 'message' && item.followUp}
      >
        {content}
      </TranscriptRow>
    </div>
  )
}

type TranscriptItem =
  | {
      kind: 'message'
      key: string
      turnId: string | null
      message: AgentSession['messages'][number]
      index: number
    }
  | {
      kind: 'block'
      key: string
      turnId: string | null
      block: AgentSession['transcript_blocks'][number]
      index: number
    }

function transcriptRows(session: AgentSession): TranscriptItem[] {
  const blocks = new Map<number, Array<{ block: AgentSession['transcript_blocks'][number]; index: number }>>()
  session.transcript_blocks.forEach((block, index) => {
    const anchor = Math.min(block.after_message, session.messages.length)
    const at = blocks.get(anchor) ?? []
    at.push({ block, index })
    blocks.set(anchor, at)
  })
  const rows: TranscriptItem[] = []
  for (let index = 0; index <= session.messages.length; index++) {
    for (const entry of blocks.get(index) ?? []) {
      rows.push({
        kind: 'block',
        key: `block-${entry.index}`,
        turnId: entry.block.turn_id ?? null,
        block: entry.block,
        index: entry.index,
      })
    }
    const message = session.messages[index]
    if (message) {
      rows.push({
        kind: 'message',
        key: `message-${message.id}`,
        turnId: message.turn_id ?? null,
        message,
        index,
      })
    }
  }
  return rows
}

function turnFolds(session: AgentSession, rows: TranscriptItem[]) {
  const hidden = new Set<string>()
  const anchors = new Map<string, { turnId: string; turn: AgentSession['turns'][number] }>()
  const rowsByTurn = new Map<string, TranscriptItem[]>()
  for (const row of rows) {
    if (!row.turnId || (row.kind === 'message' && row.message.role !== 'assistant')) continue
    const turnRows = rowsByTurn.get(row.turnId) ?? []
    turnRows.push(row)
    rowsByTurn.set(row.turnId, turnRows)
  }
  for (const turn of session.turns) {
    if (turn.status === 'running') continue
    const turnRows = rowsByTurn.get(turn.id) ?? []
    const answerStart = turnAnswerStart(turnRows, (row) => (
      row.kind === 'message' && Boolean(row.message.content.trim())
    ))
    const work = turnRows.slice(0, answerStart)
    if (!work.length) continue
    anchors.set(work[0]!.key, { turnId: turn.id, turn })
    for (const row of work) hidden.add(row.key)
  }
  return { hidden, anchors }
}

function TurnFold({
  label,
  expanded,
  onToggle,
}: {
  label: string
  expanded: boolean
  onToggle: () => void
}) {
  return (
    <div className="flex h-6 w-full items-center gap-2.5">
      <div className="h-px flex-1 bg-border" />
      <button
        aria-expanded={expanded}
        className="flex h-6 shrink-0 items-center gap-1 rounded px-0.5 text-[11.5px] font-medium leading-4 text-[var(--text-tertiary)] outline-none hover:text-[var(--text-secondary)] focus-visible:ring-1 focus-visible:ring-ring"
        type="button"
        onClick={onToggle}
      >
        {label}
        <WakuIcon className="size-2.5" name={expanded ? 'chevronDown' : 'chevronRight'} />
      </button>
      <div className="h-px flex-1 bg-border" />
    </div>
  )
}

function WorkingIndicator({ startedAt, t }: { startedAt: number; t: Translator }) {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1_000))
  useEffect(() => {
    let timeout = 0
    const tick = () => {
      setNow(Math.floor(Date.now() / 1_000))
      timeout = window.setTimeout(tick, 1_000 - (Date.now() % 1_000) + 8)
    }
    timeout = window.setTimeout(tick, 1_000 - (Date.now() % 1_000) + 8)
    return () => window.clearTimeout(timeout)
  }, [startedAt])
  return (
    <div className="flex h-[22px] items-center gap-2 text-[11.5px] text-[var(--text-tertiary)]">
      <span className="flex items-center gap-[3px]" aria-hidden="true">
        <span className="size-1 animate-pulse rounded-full bg-current motion-reduce:animate-none" />
        <span className="size-1 animate-pulse rounded-full bg-current [animation-delay:120ms] motion-reduce:animate-none" />
        <span className="size-1 animate-pulse rounded-full bg-current [animation-delay:240ms] motion-reduce:animate-none" />
      </span>
      <span className="font-medium">
        {t('transcript.working_for', {
          duration: formatWorkingElapsed(Math.max(0, now - startedAt), t),
        })}
      </span>
    </div>
  )
}

function TranscriptRow({
  children,
  first = false,
  followUp = false,
}: {
  children: ReactNode
  first?: boolean
  followUp?: boolean
}) {
  return (
    <div className={cn('py-2', first && 'pt-[22px]', followUp && 'pt-8')}>
      <div className="min-w-0">{children}</div>
    </div>
  )
}

function MessageRow({
  message,
  locale,
  t,
  footer,
  beforeFooter,
  onCopyToComposer,
  forkAction,
  rewindAction,
  editing = false,
  editContent,
  onCancelEdit,
  onSubmitEdit,
}: {
  message: AgentSession['messages'][number]
  locale: AppLocale
  t: Translator
  footer: AssistantResponseFooter | null
  beforeFooter?: ReactNode
  onCopyToComposer?: (content: string) => void
  forkAction?: ResponseForkAction
  rewindAction?: MessageRewindAction
  editing?: boolean
  editContent?: string
  onCancelEdit?: () => void
  onSubmitEdit?: (prompt: string) => Promise<void>
}) {
  const visible = message.display_content ?? message.content
  const copyContent = footer?.content ?? visible
  if (message.role === 'user') {
    return (
      <MessageContextMenu
        content={copyContent}
        t={t}
        copyToComposer={!rewindAction && visible && onCopyToComposer
          ? () => onCopyToComposer(visible)
          : undefined}
        rewindAction={rewindAction}
      >
        <article className="group/message flex w-full flex-col items-end gap-1">
          {message.attachments?.length ? (
            <div className="flex max-w-[540px] flex-wrap justify-end gap-2">
              {message.attachments.map((attachment) => (
                <Attachment key={`${attachment.blob_reference}-${attachment.name}`} attachment={attachment} />
              ))}
            </div>
          ) : null}
          {editing && onCancelEdit && onSubmitEdit ? (
            <MessageEditBubble
              attachments={message.attachments ?? []}
              initialContent={editContent ?? visible}
              pending={rewindAction?.pending ?? false}
              t={t}
              onCancel={onCancelEdit}
              onSubmit={onSubmitEdit}
            />
          ) : visible ? (
            <div className="max-w-[540px] min-w-0 rounded-xl bg-[var(--raised)] px-3 py-2 text-[14px] leading-5">
              <Markdown text={visible} compact />
            </div>
          ) : null}
          {!editing && (
            <MessageFooter
              alignRight
              content={copyContent}
              rewindAction={rewindAction}
              locale={locale}
              t={t}
              timestamp={message.created_at}
            />
          )}
        </article>
      </MessageContextMenu>
    )
  }
  if (message.role === 'system') {
    return (
      <div className="flex justify-center">
        <div className="rounded-full bg-accent px-2.5 py-1 text-[11px] leading-4 text-[var(--text-tertiary)]">
          {visible}
        </div>
      </div>
    )
  }
  return (
    <MessageContextMenu content={copyContent} forkAction={forkAction} t={t}>
      <article className="group/message min-w-0 py-1">
        <Markdown streaming={message.streaming} text={visible} />
        {beforeFooter && <div className="mb-[3px] mt-3 w-full">{beforeFooter}</div>}
        {footer && (
          <MessageFooter
            content={footer.content}
            forkAction={forkAction}
            locale={locale}
            t={t}
            timestamp={footer.timestamp}
          />
        )}
      </article>
    </MessageContextMenu>
  )
}

function MessageEditBubble({
  initialContent,
  attachments,
  pending,
  t,
  onCancel,
  onSubmit,
}: {
  initialContent: string
  attachments: MessageAttachment[]
  pending: boolean
  t: Translator
  onCancel: () => void
  onSubmit: (prompt: string) => Promise<void>
}) {
  const [content, setContent] = useState(initialContent)
  const input = useRef<HTMLTextAreaElement>(null)
  const canSubmit = Boolean(content.trim() || attachments.length)

  function resizeInput(element: HTMLTextAreaElement) {
    element.style.height = '0px'
    element.style.height = `${Math.min(200, Math.max(40, element.scrollHeight))}px`
  }

  useEffect(() => {
    if (input.current) resizeInput(input.current)
  }, [])

  function submit() {
    if (!canSubmit || pending) return
    void onSubmit(content)
  }

  return (
    <form
      className="w-full max-w-[540px] rounded-xl bg-[var(--raised)] px-3 pb-2 pt-[9px]"
      onSubmit={(event) => {
        event.preventDefault()
        submit()
      }}
    >
      <textarea
        aria-label={t('transcript.edit_message')}
        autoFocus
        className="block min-h-10 w-full resize-none overflow-y-auto bg-transparent text-[14px] leading-5 outline-none placeholder:text-[var(--text-ghost)]"
        disabled={pending}
        ref={input}
        rows={1}
        value={content}
        onChange={(event) => {
          setContent(event.target.value)
          resizeInput(event.target)
        }}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            event.preventDefault()
            if (!pending) onCancel()
          } else if (
            event.key === 'Enter'
            && !event.shiftKey
            && !event.nativeEvent.isComposing
          ) {
            event.preventDefault()
            submit()
          }
        }}
      />
      <div className="mt-[7px] flex justify-end gap-1.5">
        <button
          className="h-[26px] rounded-[7px] border bg-card px-2.5 text-[11.5px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-45"
          disabled={pending}
          type="button"
          onClick={onCancel}
        >
          {t('common.cancel')}
        </button>
        <button
          className="flex h-[26px] items-center gap-1.5 rounded-[7px] bg-foreground px-[11px] text-[11.5px] font-medium text-background outline-none hover:opacity-90 focus-visible:ring-1 focus-visible:ring-ring disabled:bg-accent disabled:text-[var(--text-ghost)] disabled:opacity-100"
          disabled={!canSubmit || pending}
          type="submit"
        >
          {pending && <WakuIcon className="size-3 motion-safe:animate-spin" name="loaderCircle" />}
          {t('common.send')}
        </button>
      </div>
    </form>
  )
}

function MessageFooter({
  content,
  locale,
  t,
  timestamp,
  alignRight = false,
  forkAction,
  rewindAction,
}: {
  content: string
  locale: AppLocale
  t: Translator
  timestamp: number
  alignRight?: boolean
  forkAction?: ResponseForkAction
  rewindAction?: MessageRewindAction
}) {
  const [copied, setCopied] = useState(false)
  const copiedTimeout = useRef<number | null>(null)
  useEffect(() => () => {
    if (copiedTimeout.current !== null) window.clearTimeout(copiedTimeout.current)
  }, [])
  return (
    <div className={cn(
      'flex h-[27px] items-center gap-px text-[11.5px] text-[var(--text-ghost)] opacity-0 transition-opacity group-hover/message:opacity-100 group-focus-within/message:opacity-100 motion-reduce:transition-none',
      alignRight && 'justify-end',
      !alignRight && '-ml-[7px]',
    )}>
      {alignRight && <span className="flex h-[27px] items-center px-1">{formatMessageTime(timestamp, undefined, locale)}</span>}
      <button
        aria-label={t(copied ? 'common.copied' : 'common.copy_message')}
        className="grid size-[27px] place-items-center rounded-lg outline-none hover:bg-accent hover:text-[var(--text-secondary)] focus-visible:opacity-100 focus-visible:ring-1 focus-visible:ring-ring"
        title={t(copied ? 'common.copied' : 'common.copy_message')}
        type="button"
        onClick={() => {
          void navigator.clipboard.writeText(content)
          setCopied(true)
          if (copiedTimeout.current !== null) window.clearTimeout(copiedTimeout.current)
          copiedTimeout.current = window.setTimeout(() => {
            setCopied(false)
            copiedTimeout.current = null
          }, 2_000)
        }}
      >
        <WakuIcon className="size-3.5" name={copied ? 'check' : 'copy'} />
      </button>
      {alignRight && rewindAction && (
        <button
          aria-label={t(rewindAction.pending ? 'session.reverting_message' : 'session.revert_to_here')}
          className="grid size-[27px] place-items-center rounded-lg outline-none hover:bg-accent hover:text-[var(--text-secondary)] focus-visible:opacity-100 focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-45"
          disabled={rewindAction.pending}
          title={t(rewindAction.pending ? 'session.reverting_message' : 'session.revert_to_here')}
          type="button"
          onClick={rewindAction.onBegin}
        >
          <WakuIcon
            className={cn('size-3.5', rewindAction.pending && 'motion-safe:animate-spin')}
            name={rewindAction.pending ? 'loaderCircle' : 'rewind'}
          />
        </button>
      )}
      {!alignRight && forkAction && (
        <button
          aria-label={t(forkAction.pending ? 'session.forking_task' : 'session.fork_task')}
          className="grid size-[27px] place-items-center rounded-lg outline-none hover:bg-accent hover:text-[var(--text-secondary)] focus-visible:opacity-100 focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-45"
          disabled={forkAction.pending}
          title={t(forkAction.pending ? 'session.forking_task' : 'session.fork_task')}
          type="button"
          onClick={() => forkAction.onFork(forkAction.turnCount)}
        >
          <WakuIcon
            className={cn('size-3.5', forkAction.pending && 'motion-safe:animate-spin')}
            name={forkAction.pending ? 'loaderCircle' : 'fork'}
          />
        </button>
      )}
      {!alignRight && <span className="flex h-[27px] items-center px-1">{formatMessageTime(timestamp, undefined, locale)}</span>}
    </div>
  )
}

function MessageContextMenu({
  children,
  content,
  t,
  copyToComposer,
  forkAction,
  rewindAction,
}: {
  children: ReactNode
  content: string
  t: Translator
  copyToComposer?: () => void
  forkAction?: ResponseForkAction
  rewindAction?: MessageRewindAction
}) {
  const code = fencedCode(content)
  const [selectedText, setSelectedText] = useState('')
  return (
    <ContextMenu.Root onOpenChange={(open) => {
      if (open) setSelectedText(window.getSelection()?.toString() ?? '')
    }}>
      <ContextMenu.Trigger className="block w-full min-w-0 outline-none">
        {children}
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Positioner className="z-[100] outline-none">
          <ContextMenu.Popup
            className="waku-menu-surface"
            finalFocus={false}
          >
            {selectedText && (
              <ContextMenu.Item
                className="waku-menu-item"
                onClick={() => void navigator.clipboard.writeText(selectedText)}
              >
                <WakuIcon className="size-3" name="copy" /> {t('common.copy_selection')}
              </ContextMenu.Item>
            )}
            <ContextMenu.Item
              className="waku-menu-item"
              onClick={() => void navigator.clipboard.writeText(content)}
            >
              <WakuIcon className="size-3" name="copy" /> {t('common.copy_message_title')}
            </ContextMenu.Item>
            {copyToComposer && (
              <ContextMenu.Item
                className="waku-menu-item"
                onClick={copyToComposer}
              >
                <WakuIcon className="size-3" name="compose" /> {t('common.copy_to_composer')}
              </ContextMenu.Item>
            )}
            {code && (
              <ContextMenu.Item
                className="waku-menu-item"
                onClick={() => void navigator.clipboard.writeText(code)}
              >
                <WakuIcon className="size-3" name="copy" /> {t('common.copy_code')}
              </ContextMenu.Item>
            )}
            {rewindAction && (
              <>
                <ContextMenu.Separator className="waku-menu-separator" />
                <ContextMenu.Item
                  className="waku-menu-item"
                  disabled={rewindAction.pending}
                  onClick={rewindAction.onBegin}
                >
                  <WakuIcon
                    className={cn('size-3', rewindAction.pending && 'motion-safe:animate-spin')}
                    name={rewindAction.pending ? 'loaderCircle' : 'rewind'}
                  />
                  {t(rewindAction.pending ? 'session.reverting_message_title' : 'session.revert_to_here_title')}
                </ContextMenu.Item>
              </>
            )}
            {forkAction && (
              <>
                <ContextMenu.Separator className="waku-menu-separator" />
                <ContextMenu.Item
                  className="waku-menu-item"
                  disabled={forkAction.pending}
                  onClick={() => forkAction.onFork(forkAction.turnCount)}
                >
                  <WakuIcon
                    className={cn('size-3', forkAction.pending && 'motion-safe:animate-spin')}
                    name={forkAction.pending ? 'loaderCircle' : 'fork'}
                  />
                  {t(forkAction.pending ? 'session.forking_task_title' : 'session.fork_task_title')}
                </ContextMenu.Item>
              </>
            )}
          </ContextMenu.Popup>
        </ContextMenu.Positioner>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  )
}

function Markdown({
  text,
  compact = false,
  streaming = false,
}: {
  text: string
  compact?: boolean
  streaming?: boolean
}) {
  const onOpenLink = useContext(TranscriptLinkContext)
  // Match the painter's attach semantics: text already present when this row
  // mounts is the baseline; only later appends dissolve in.
  const veil = useRef(createMarkdownVeilState(text))
  const now = Date.now()
  const chunks = advanceMarkdownVeil(veil.current, text, streaming, now)
  return (
    <div className={cn('markdown min-w-0', compact && '[&>*:first-child]:mt-0 [&>*:last-child]:mb-0')}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={chunks.length ? [markdownVeilPlugin(chunks, now)] : []}
        components={{
          a: ({ children, href, ...props }) => (
            <a
              {...props}
              href={href}
              target="_blank"
              rel="noreferrer noopener"
              onClick={(event) => {
                if (href && onOpenLink(href)) event.preventDefault()
              }}
            >
              {children}
            </a>
          ),
          img: ({ alt, src }) => typeof src === 'string' ? (
            <PreviewableImage
              buttonClassName="max-w-full rounded-[9px] border bg-[var(--inset)]"
              imageClassName="max-h-64 max-w-full object-contain"
              name={alt || imageName(src)}
              source={src}
            />
          ) : null,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}

function ActivityGroup({
  activities,
  backgroundWork,
  liveTurn,
  t,
  onOpenBackgroundWork,
}: {
  activities: ActivityItem[]
  backgroundWork: BackgroundWorkItem[]
  liveTurn: boolean
  t: Translator
  onOpenBackgroundWork?: (key: BackgroundWorkKey) => void
}) {
  const running = activities.some((activity) => !activity.complete)
  const [expanded, setExpanded] = useState(running || liveTurn)
  useEffect(() => {
    if (running || liveTurn) setExpanded(true)
  }, [running, liveTurn])
  if (!activities.length) return null
  return (
    <div className="min-w-0 text-[12px] text-[var(--text-tertiary)]">
      <button
        aria-expanded={expanded}
        className="flex h-7 w-full min-w-0 items-center gap-1.5 rounded outline-none hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring"
        type="button"
        onClick={() => setExpanded((value) => !value)}
      >
        <span className="min-w-0 truncate text-left text-[12.5px] font-medium text-[var(--text-secondary)]">{activityHeaderTitle(activities, liveTurn, t)}</span>
        <WakuIcon className="size-2.5 shrink-0" name={expanded ? 'chevronDown' : 'chevronRight'} />
      </button>
      {expanded && (
        <div className="ml-1.5 flex min-w-0 flex-col gap-2 border-l pb-0.5 pl-3">
          {activities.map((activity) => (
            <ActivityRow
              activity={activity}
              backgroundWork={backgroundWork.find((item) => (
                item.originActivityId === activity.source_id
              ))}
              key={activity.id}
              t={t}
              onOpenBackgroundWork={onOpenBackgroundWork}
            />
          ))}
        </div>
      )}
    </div>
  )
}

function ActivityRow({
  activity,
  backgroundWork,
  t,
  onOpenBackgroundWork,
}: {
  activity: ActivityItem
  backgroundWork?: BackgroundWorkItem
  t: Translator
  onOpenBackgroundWork?: (key: BackgroundWorkKey) => void
}) {
  const sections = activity.reasoning ? [] : activityDisclosureSections(activity, t)
  const reasoningContent = activity.reasoning?.content.trim() ?? ''
  const hasDetail = Boolean(reasoningContent || sections.length)
  const [expanded, setExpanded] = useState(Boolean(activity.reasoning && !activity.complete))
  const iconName = activityIcon(activity)
  const preview = expanded || activity.reasoning ? '' : activityPreview(activity, t)
  const actionLabel = activityActionLabel(activity, t)
  const rowDetail = activityRowDetail(activity, t) || preview
  const fileStats = activityFileChangeStats(activity)
  const scrollContent = reasoningContent || (activity.kind === 'command'
    ? sections.find((section) => section.kind === 'output')?.content ?? ''
    : '')
  const detailScroll = useRef<HTMLDivElement>(null)
  const detailFollowsTail = useRef(true)
  const [detailEdges, setDetailEdges] = useState({ atTop: true, atBottom: true })
  const updateDetailEdges = useCallback((viewport: HTMLDivElement) => {
    const atTop = viewport.scrollTop <= 1
    const atBottom = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight <= 1
    detailFollowsTail.current = atBottom
    setDetailEdges((current) => (
      current.atTop === atTop && current.atBottom === atBottom
        ? current
        : { atTop, atBottom }
    ))
  }, [])
  useEffect(() => {
    if (!expanded || !scrollContent) return
    const viewport = detailScroll.current
    if (!viewport) return
    if (!activity.complete && detailFollowsTail.current) {
      viewport.scrollTop = viewport.scrollHeight
    }
    updateDetailEdges(viewport)
  }, [activity.complete, expanded, scrollContent, updateDetailEdges])
  return (
    <div className="min-w-0 overflow-hidden rounded-[9px] border bg-[var(--activity-surface)]">
      <div className="flex h-7 w-full min-w-0 items-center">
        <button
          aria-expanded={hasDetail ? expanded : undefined}
          className={cn(
            'flex h-7 min-w-0 flex-1 items-center gap-2 px-2 text-left outline-none',
            expanded ? 'rounded-t-[8px]' : 'rounded-[8px]',
            hasDetail && 'hover:bg-[var(--activity-hover-surface)] active:bg-[var(--activity-active-surface)] focus-visible:bg-[var(--activity-hover-surface)] focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring',
          )}
          disabled={!hasDetail}
          type="button"
          onClick={() => setExpanded((value) => !value)}
        >
          <WakuIcon className="size-3 shrink-0 text-[var(--text-tertiary)]" name={iconName} />
          <span className="shrink-0 font-semibold text-[var(--text-secondary)]">{actionLabel}</span>
          {rowDetail && (
            <span className="min-w-0 flex-1 truncate text-[var(--text-secondary)]">
              {rowDetail}
            </span>
          )}
          {fileStats && (
            <>
              <span className="shrink-0 text-[var(--success)]">+{fileStats.additions}</span>
              <span className="shrink-0 text-destructive">-{fileStats.deletions}</span>
            </>
          )}
          <ActivityState activity={activity} expanded={expanded} hasDetail={hasDetail} t={t} />
        </button>
        {backgroundWork && onOpenBackgroundWork && (
          <button
            className={cn(
              'mr-2 flex h-5 shrink-0 items-center rounded-[5px] border px-1.5 text-[9.5px] outline-none hover:bg-accent focus-visible:border-ring',
              backgroundWorkStatusClass(backgroundWork.status),
            )}
            type="button"
            onClick={(event) => {
              event.stopPropagation()
              onOpenBackgroundWork(backgroundWork.key)
            }}
          >
            {t(`background.status.${backgroundWork.status}`)}
          </button>
        )}
      </div>
      {expanded && hasDetail && (
        activity.reasoning ? (
          <div className="border-t">
            <ActivityScrollableContent
              className="px-3 py-2 text-[var(--text-secondary)]"
              edges={detailEdges}
              viewportRef={detailScroll}
              onScroll={updateDetailEdges}
            >
              <Markdown streaming={!activity.complete} text={reasoningContent} />
            </ActivityScrollableContent>
          </div>
        ) : (
          <div className="flex min-w-0 flex-col gap-2 overflow-hidden border-t px-3 py-2 font-mono text-[10.5px] leading-4 text-[var(--text-secondary)]">
            {sections.map((section) => (
              <ActivitySection
                edges={detailEdges}
                key={section.kind}
                scrollable={activity.kind === 'command' && section.kind === 'output'}
                section={section}
                t={t}
                viewportRef={detailScroll}
                onScroll={updateDetailEdges}
              />
            ))}
            {activity.image_urls?.map((url, index) => (
              <ActivityImage key={`${url}-${index}`} reference={url} t={t} />
            ))}
          </div>
        )
      )}
    </div>
  )
}

type ActivityScrollEdges = { atTop: boolean; atBottom: boolean }

function ActivityScrollableContent({
  children,
  className,
  edges,
  viewportRef,
  onScroll,
}: {
  children: ReactNode
  className?: string
  edges: ActivityScrollEdges
  viewportRef: RefObject<HTMLDivElement | null>
  onScroll: (viewport: HTMLDivElement) => void
}) {
  return (
    <div className="relative min-w-0 overflow-hidden">
      <div
        className={cn(
          'max-h-[400px] min-w-0 overflow-y-auto overscroll-contain',
          className,
        )}
        onScroll={(event) => onScroll(event.currentTarget)}
        ref={viewportRef}
      >
        {children}
      </div>
      <div
        aria-hidden="true"
        className={cn(
          'pointer-events-none absolute left-0 right-2 top-0 h-5 bg-gradient-to-b from-[var(--activity-surface)] to-transparent transition-opacity motion-reduce:transition-none',
          edges.atTop ? 'opacity-0' : 'opacity-100',
        )}
      />
      <div
        aria-hidden="true"
        className={cn(
          'pointer-events-none absolute bottom-0 left-0 right-2 h-5 bg-gradient-to-t from-[var(--activity-surface)] to-transparent transition-opacity motion-reduce:transition-none',
          edges.atBottom ? 'opacity-0' : 'opacity-100',
        )}
      />
    </div>
  )
}

function backgroundWorkStatusClass(status: BackgroundWorkStatus) {
  if (status === 'starting' || status === 'running' || status === 'monitoring') {
    return 'text-ring'
  }
  if (status === 'completed') return 'text-[var(--success)]'
  if (status === 'failed' || status === 'lost') return 'text-destructive'
  return 'text-[var(--text-tertiary)]'
}

function ActivitySection({
  edges,
  scrollable,
  section,
  t,
  viewportRef,
  onScroll,
}: {
  edges: ActivityScrollEdges
  scrollable: boolean
  section: ReturnType<typeof activityDisclosureSections>[number]
  t: Translator
  viewportRef: RefObject<HTMLDivElement | null>
  onScroll: (viewport: HTMLDivElement) => void
}) {
  const [copied, setCopied] = useState(false)
  const copiedTimeout = useRef<number | null>(null)
  useEffect(() => () => {
    if (copiedTimeout.current !== null) window.clearTimeout(copiedTimeout.current)
  }, [])
  return (
    <div className="flex min-w-0 flex-col gap-[3px]">
      {section.label && (
        <div className="flex h-5 items-center justify-between font-medium">
          <span>{section.label}</span>
          {section.content && (
            <button
              aria-label={copied
                ? t('common.copied')
                : t('common.copy_named', { name: section.label })}
              className="grid size-5 place-items-center rounded-[5px] text-[var(--text-ghost)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring"
              type="button"
              onClick={() => {
                void navigator.clipboard.writeText(section.content)
                setCopied(true)
                if (copiedTimeout.current !== null) window.clearTimeout(copiedTimeout.current)
                copiedTimeout.current = window.setTimeout(() => {
                  setCopied(false)
                  copiedTimeout.current = null
                }, 2_000)
              }}
            >
              <WakuIcon className="size-[11px]" name={copied ? 'check' : 'copy'} />
            </button>
          )}
        </div>
      )}
      {section.content && (
        scrollable ? (
          <ActivityScrollableContent
            className="py-1 pr-2"
            edges={edges}
            viewportRef={viewportRef}
            onScroll={onScroll}
          >
            <pre className="min-w-0 whitespace-pre-wrap break-words font-mono text-[10.5px] leading-4">
              {section.content}
            </pre>
          </ActivityScrollableContent>
        ) : section.kind === 'command' ? (
          <pre className="min-w-0 whitespace-pre-wrap break-words font-mono text-[10.5px] leading-4">
            {section.content}
          </pre>
        ) : (
          <ActivitySectionText content={section.content} label={section.label} t={t} />
        )
      )}
    </div>
  )
}

function ActivitySectionText({
  content,
  label,
  t,
}: {
  content: string
  label: string | null
  t: Translator
}) {
  const rows = activityTextRows(content)
  if (!shouldVirtualizeActivityText(content, rows)) {
    return (
      <pre className="max-h-72 min-w-0 overflow-auto whitespace-pre-wrap break-words font-mono text-[10.5px] leading-4">
        {content}
      </pre>
    )
  }
  return (
    <Virtuoso
      aria-label={label
        ? t('activity.named_content', { name: label })
        : t('activity.detail')}
      className="h-72 min-w-0 overscroll-contain font-mono text-[10.5px] leading-4"
      data={rows}
      defaultItemHeight={16}
      increaseViewportBy={128}
      itemContent={(_, row) => (
        <div className="min-h-4 whitespace-pre-wrap break-words">{row || '\u00a0'}</div>
      )}
    />
  )
}

function ActivityState({
  activity,
  expanded,
  hasDetail,
  t,
}: {
  activity: ActivityItem
  expanded: boolean
  hasDetail: boolean
  t: Translator
}) {
  if (hasDetail) return <WakuIcon className="size-2.5 text-[var(--text-tertiary)]" name={expanded ? 'chevronDown' : 'chevronRight'} />
  if (activity.reasoning) return null
  if (activity.failed) return <WakuIcon label={t('background.status.failed')} className="size-3 text-destructive" name="alert" />
  if (activity.complete) return null
  return <span aria-label={t('background.status.running')} className="size-1.5 rounded-full bg-ring motion-safe:animate-pulse" role="img" />
}

function ChangedFilesCard({
  files,
  additions,
  deletions,
  t,
  onReview,
}: {
  files: Array<{ path: string; additions: number; deletions: number }>
  additions: number
  deletions: number
  t: Translator
  onReview?: () => void
}) {
  const [expanded, setExpanded] = useState(false)
  const shown = expanded ? files.slice(0, 12) : files.slice(0, 3)
  const clipped = expanded && files.length > 12
  return (
    <div className="overflow-hidden rounded-xl border bg-accent">
      <div className="flex min-h-[58px] items-center gap-2.5 px-3 py-[9px]">
        <span className="grid size-9 shrink-0 place-items-center rounded-[9px] bg-[var(--raised)]">
          <WakuIcon className="size-4 text-[var(--text-tertiary)]" name="fileDiff" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[12.5px] font-medium">
            {t(files.length === 1 ? 'transcript.changed_file' : 'transcript.changed_files', {
              count: files.length,
            })}
          </div>
          <div className="mt-0.5 flex gap-1.5 text-[11px] leading-[14px]">
            <span className="text-[var(--success)]">+{additions}</span>{' '}
            <span className="text-destructive">-{deletions}</span>
          </div>
        </div>
        {onReview && (
          <button
            className="flex h-7 items-center gap-1 rounded-[7px] border bg-card px-2.5 text-[11.5px] font-medium text-[var(--text-secondary)] outline-none hover:bg-[var(--raised)] focus-visible:border-ring"
            type="button"
            onClick={onReview}
          >
            <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="fileDiff" />
            {t('transcript.review_changes')}
          </button>
        )}
      </div>
      <div className="border-t">
        {shown.map((file) => (
          <div className="flex h-[31px] items-center gap-2 px-3 text-[11.5px]" key={file.path}>
            <span className="min-w-0 flex-1 truncate text-[var(--text-secondary)]" title={file.path}>{file.path}</span>
            <span className="text-[10.5px] text-[var(--success)]">+{file.additions}</span>
            <span className="text-[10.5px] text-destructive">-{file.deletions}</span>
          </div>
        ))}
        {files.length > 3 && (
          <button
            className="flex h-[34px] w-full items-center gap-1.5 border-t px-3 text-left text-[11.5px] font-medium text-[var(--text-secondary)] outline-none hover:bg-[var(--raised)] focus-visible:bg-[var(--raised)]"
            type="button"
            onClick={() => setExpanded((value) => !value)}
          >
            <span>{expanded
              ? t('transcript.show_fewer_files')
              : t('transcript.show_more_files', { count: files.length - 3 })}</span>
            {clipped && (
              <span className="min-w-0 flex-1 truncate font-normal text-[var(--text-ghost)]">
                {t('transcript.showing_first_files', { count: 12, total: files.length })}
              </span>
            )}
            <span className="flex-1" />
            <WakuIcon className="size-[11px] text-[var(--text-tertiary)]" name={expanded ? 'chevronDown' : 'chevronRight'} />
          </button>
        )}
      </div>
    </div>
  )
}

function Attachment({ attachment }: { attachment: MessageAttachment }) {
  if (attachment.is_image) return <RemoteImage attachment={attachment} />
  return (
    <span
      className="flex h-20 w-24 flex-col items-center justify-center gap-[7px] overflow-hidden rounded-[9px] border bg-[var(--inset)] px-[7px]"
      title={attachment.name}
    >
      {attachment.is_dir
        ? <WakuIcon className="size-[18px] text-[var(--text-tertiary)]" name="folder" />
        : <FileTypeIcon className="size-[18px]" path={attachment.mention || attachment.name} />}
      <span className="w-full truncate text-center text-[9.5px] text-[var(--text-secondary)]">
        {attachment.name}
      </span>
    </span>
  )
}

function RemoteImage({
  attachment,
  tile = true,
}: {
  attachment: MessageAttachment
  tile?: boolean
}) {
  const { client, config, phase } = useDaemon()
  const [source, setSource] = useState<string | null>(null)
  useEffect(() => {
    if (phase !== 'connected' || !client || !config || !attachment.blob_reference) {
      setSource(null)
      return
    }
    let active = true
    void readAttachmentImage(client, attachment)
      .then((value) => active && setSource(value))
      .catch(() => active && setSource(null))
    return () => { active = false }
  }, [client, config?.address, phase, attachment.blob_reference, attachment.path, attachment.name])

  if (!source) {
    return (
      <span
        className={cn(
          'grid place-items-center rounded-[9px] border bg-[var(--inset)]',
          tile ? 'h-20 w-24' : 'h-24 w-full',
        )}
        title={attachment.name}
      >
        <FileTypeIcon className="size-[18px] opacity-60" path={attachment.name} />
      </span>
    )
  }
  return (
    <PreviewableImage
      buttonClassName={cn(
        'rounded-[9px] border bg-[var(--inset)] focus-visible:border-ring',
        tile ? 'h-20 w-24' : 'max-w-full',
      )}
      imageClassName={tile ? 'size-full object-cover' : 'max-h-64 max-w-full object-contain'}
      name={attachment.name}
      source={source}
    />
  )
}

function ActivityImage({ reference, t }: { reference: string; t: Translator }) {
  if (reference.startsWith('data:') || reference.startsWith('https://')) {
    return (
      <PreviewableImage
        buttonClassName="max-w-full rounded-lg border"
        imageClassName="max-h-64 max-w-full object-contain"
        name={t('activity.image_output')}
        source={reference}
      />
    )
  }
  return (
    <RemoteImage
      tile={false}
      attachment={{
        path: reference,
        mention: reference,
        name: reference,
        is_dir: false,
        is_image: true,
        blob_reference: reference,
      }}
    />
  )
}

function activityIcon(activity: ActivityItem): WakuIconName {
  if (activity.reasoning || activity.kind === 'reasoning') return 'sparkle'
  if (activity.kind === 'command') return 'terminal'
  if (activity.kind === 'search' || activity.kind === 'fileSearch') return 'search'
  if (activity.kind === 'fileRead') return 'file'
  if (activity.kind === 'fileChange') return 'pencil'
  if (activity.kind === 'fileList') return 'folder'
  if (activity.kind === 'plan') return 'list'
  return 'wrench'
}

function lastIndexWhere<T>(values: readonly T[], predicate: (value: T) => boolean) {
  for (let index = values.length - 1; index >= 0; index--) {
    if (predicate(values[index]!)) return index
  }
  return -1
}

function imageName(source: string) {
  if (source.startsWith('data:')) return 'Image'
  try {
    const path = new URL(source, window.location.href).pathname
    return decodeURIComponent(path.split('/').filter(Boolean).at(-1) ?? 'Image')
  } catch {
    return source.split('/').filter(Boolean).at(-1) ?? 'Image'
  }
}
