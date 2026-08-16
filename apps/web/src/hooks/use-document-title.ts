import { useEffect } from 'react'

export const WAKU_DOCUMENT_TITLE = 'Waku Web'

export function formatDocumentTitle(section?: string | null): string {
  const normalized = section?.trim()
  if (!normalized || normalized === WAKU_DOCUMENT_TITLE) return WAKU_DOCUMENT_TITLE
  return `${normalized} — ${WAKU_DOCUMENT_TITLE}`
}

export function useDocumentTitle(section?: string | null) {
  const title = formatDocumentTitle(section)
  useEffect(() => {
    document.title = title
  }, [title])
}
