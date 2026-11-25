import { useSyncExternalStore } from 'react'

/**
 * Whether the web client renders math instead of its LaTeX source.
 *
 * The setting is a per-browser preference, the way the theme and the sidebar
 * width are. Math is on by default, the way it is on the desktop: a model
 * that writes a formula usually means for it to be read as one.
 *
 * It exists because a dollar sign is ordinary punctuation. `$5 and $10` is a
 * price, but a math extension reads the first pair as a formula — so the
 * choice has to be the user's.
 */
const STORAGE_KEY = 'waku.render-math'
const EVENT = 'waku:render-math'

/** Whether math rendering is on. */
export function readRenderMath(): boolean {
  if (typeof window === 'undefined') return true
  return window.localStorage.getItem(STORAGE_KEY) !== 'false'
}

/** Turn math rendering on or off, and tell the transcript about it. */
export function writeRenderMath(value: boolean) {
  if (typeof window === 'undefined') return
  window.localStorage.setItem(STORAGE_KEY, String(value))
  window.dispatchEvent(new CustomEvent(EVENT))
}

function subscribe(listener: () => void) {
  window.addEventListener(EVENT, listener)
  return () => window.removeEventListener(EVENT, listener)
}

/** The current math-rendering choice, kept in step with the settings view. */
export function useRenderMath(): boolean {
  return useSyncExternalStore(subscribe, readRenderMath, () => true)
}
