import { queryOptions } from '@tanstack/react-query'
import { createServerFn } from '@tanstack/react-start'

export interface LatestRelease {
  version: string
  url: string
  pubDate: string | null
}

const RELEASES_BASE = 'https://releases.waku.sh'

// Versioned artifact names are a stable contract and old archives stay in R2
// (see RELEASING.md), so a known-published version is a safe fallback while
// the appcast query is pending or unreachable.
export const FALLBACK_DOWNLOAD_URL = `${RELEASES_BASE}/Waku-0.0.1.dmg`

export const WINDOWS_ARCHITECTURES = [
  { arch: 'x86_64', label: 'Windows (x86_64)' },
  { arch: 'aarch64', label: 'Windows (arm64)' },
] as const

// Every release publishes both installers under versioned names — see
// docs/windows.md. There is no unversioned "latest" object to link at, so a
// direct link needs the resolved version; without one the menu falls back to
// the docs page rather than guessing a URL that would 404.
export function windowsInstallerUrl(version: string, arch: string) {
  return `${RELEASES_BASE}/Waku-${version}-${arch}-Setup.exe`
}

// The Sparkle appcast has no CORS headers, so resolve it on the server.
const fetchLatestRelease = createServerFn({ method: 'GET' }).handler(
  async (): Promise<LatestRelease | null> => {
    try {
      const res = await fetch(`${RELEASES_BASE}/appcast.xml`, {
        signal: AbortSignal.timeout(2500),
      })
      if (!res.ok) return null
      const xml = await res.text()
      // generate_appcast writes the newest release first.
      const version =
        xml.match(/sparkle:shortVersionString="([^"]+)"/)?.[1] ??
        xml.match(/<sparkle:shortVersionString>([^<]+)</)?.[1]
      if (!version) return null
      const pubDate = xml.match(/<pubDate>([^<]+)<\/pubDate>/)?.[1] ?? null
      return {
        version,
        url: `${RELEASES_BASE}/Waku-${version}.dmg`,
        pubDate,
      }
    } catch {
      return null
    }
  },
)

export const releaseQuery = queryOptions({
  queryKey: ['latest-release'],
  queryFn: () => fetchLatestRelease(),
  staleTime: 5 * 60_000,
})
