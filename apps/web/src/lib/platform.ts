import { useSyncExternalStore } from 'react'

const subscribePlatform = () => () => {}

export function isMacLikePlatform(platform: string): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform)
}

export function useMacLikePlatform(): boolean {
  return useSyncExternalStore(
    subscribePlatform,
    () => isMacLikePlatform(browserPlatform()),
    () => false,
  )
}

export function usePrimaryShortcut(mac: string, other: string): string {
  return useMacLikePlatform() ? mac : other
}

function browserPlatform(): string {
  if (typeof navigator === 'undefined') return ''
  const userAgentData = (navigator as Navigator & {
    userAgentData?: { platform?: string }
  }).userAgentData
  return userAgentData?.platform || navigator.platform || navigator.userAgent
}
