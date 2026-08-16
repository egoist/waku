import { useEffect, useRef, useState } from 'react'

const COPY_FEEDBACK_TIMEOUT_MS = 2_000

export function useCopyFeedback() {
  const [copied, setCopied] = useState(false)
  const timeout = useRef<number | null>(null)

  useEffect(() => () => {
    if (timeout.current !== null) window.clearTimeout(timeout.current)
  }, [])

  async function copyText(text: string) {
    try {
      await navigator.clipboard.writeText(text)
    } catch {
      return false
    }
    setCopied(true)
    if (timeout.current !== null) window.clearTimeout(timeout.current)
    timeout.current = window.setTimeout(() => {
      setCopied(false)
      timeout.current = null
    }, COPY_FEEDBACK_TIMEOUT_MS)
    return true
  }

  return { copied, copyText }
}
