import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Menu } from '@base-ui/react/menu'
import {
  Command,
  Download,
  HardDrive,
  History,
  Layers,
  RefreshCw,
  Zap,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from '@/components/ui/accordion'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  FALLBACK_DOWNLOAD_URL,
  WINDOWS_ARCHITECTURES,
  releaseQuery,
  windowsInstallerUrl,
} from '@/lib/release'
import type { ReactNode } from 'react'

export const Route = createFileRoute('/')({
  loader: ({ context }) => {
    // Fire-and-forget: the version chip streams in when the appcast answers.
    void context.queryClient.prefetchQuery(releaseQuery)
  },
  component: Home,
})

const WINDOWS_DOCS_URL =
  'https://github.com/egoist/waku/blob/main/docs/windows.md'

const PROVIDERS = [
  { slug: 'amp', label: 'Amp' },
  { slug: 'claude', label: 'Claude Code' },
  { slug: 'openai', label: 'Codex' },
  { slug: 'cursor', label: 'Cursor' },
  { slug: 'opencode', label: 'OpenCode' },
  { slug: 'grok', label: 'Grok' },
  { slug: 'pi', label: 'Pi' },
  { slug: 'kimi', label: 'Kimi' },
]

const FEATURES = [
  {
    icon: Zap,
    title: 'Native down to the frame',
    body: 'Rust and GPUI — the GPU-accelerated framework behind Zed. Instant launch, smooth scrolling through years of transcript, no Electron.',
  },
  {
    icon: Layers,
    title: 'Every agent, one timeline',
    body: 'Each agent is connected over its strongest native interface — stream-json, JSON-RPC, live events — and normalized into one provider-neutral model.',
  },
  {
    icon: History,
    title: 'Rewind that means it',
    body: 'Every prompt checkpoints your working tree under a hidden git ref. Roll back the code and the provider conversation together, not just the chat log.',
  },
  {
    icon: Command,
    title: 'Keyboard first',
    body: '⌘N starts a session, ⏎ queues a follow-up while the agent works, ⌘⏎ steers it mid-turn, Escape stops. Every control works without a mouse.',
  },
  {
    icon: HardDrive,
    title: 'Local by architecture',
    body: 'Projects, sessions, transcripts, and provider IDs live on your disk. No account, no telemetry, no Waku cloud between you and your agents.',
  },
  {
    icon: RefreshCw,
    title: 'Quietly current',
    body: 'Signed, notarized, and auto-updated with binary deltas via Sparkle. The app stays fresh without asking for your attention.',
  },
]

const FAQ = [
  {
    q: 'Is this another Electron app?',
    a: 'No. Waku is a single Rust binary rendered by GPUI, the UI framework Zed is built on. The window you see is drawn by the GPU, not by a browser engine.',
  },
  {
    q: 'Do I need new API keys?',
    a: 'No. Waku detects amp, claude, codex, cursor-agent, opencode, grok, pi, and kimi on your machine and drives them directly — your existing logins, plans, and rate limits apply unchanged.',
  },
  {
    q: 'Where does my data live?',
    a: 'On your machine. Projects, sessions, transcripts, and provider session IDs are stored locally. There is no Waku account and no telemetry.',
  },
  {
    q: 'What is the future plan?',
    a: 'A mobile app for remote control, and cloud agents are planned',
  },
]

function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <div className="font-mono text-[11px] tracking-[0.14em] text-muted-foreground/80 uppercase">
      {children}
    </div>
  )
}

function DownloadMenu({
  downloadUrl,
  version,
  size,
  align,
  className,
  showIcon = false,
}: {
  downloadUrl: string
  version?: string
  size: 'sm' | 'lg'
  align: 'start' | 'end'
  className?: string
  showIcon?: boolean
}) {
  const itemClassName =
    'flex h-8 cursor-default items-center rounded-md px-2.5 text-sm outline-none data-highlighted:bg-accent data-highlighted:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-45'

  return (
    <Menu.Root>
      <Menu.Trigger render={<Button size={size} className={className} />}>
        {showIcon && <Download data-icon="inline-start" />}
        Download
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner
          side="bottom"
          sideOffset={6}
          align={align}
          className="isolate z-50"
        >
          <Menu.Popup className="min-w-52 origin-(--transform-origin) rounded-lg border bg-popover p-1 text-popover-foreground shadow-md outline-none data-[side=bottom]:slide-in-from-top-1 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95">
            <Menu.LinkItem
              href={downloadUrl}
              closeOnClick
              className={itemClassName}
            >
              macOS (Apple Silicon)
            </Menu.LinkItem>
            <Menu.LinkItem
              href="https://github.com/egoist/waku/blob/main/docs/linux.md"
              target="_blank"
              rel="noreferrer"
              closeOnClick
              className={itemClassName}
            >
              Linux (x86_64, arm64)
            </Menu.LinkItem>
            {WINDOWS_ARCHITECTURES.map(({ arch, label }) => (
              <Menu.LinkItem
                key={arch}
                href={
                  version
                    ? windowsInstallerUrl(version, arch)
                    : WINDOWS_DOCS_URL
                }
                closeOnClick
                className={itemClassName}
              >
                {label}
              </Menu.LinkItem>
            ))}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  )
}

function Home() {
  const { data: release } = useQuery(releaseQuery)
  const downloadUrl = release?.url ?? FALLBACK_DOWNLOAD_URL

  return (
    <TooltipProvider>
      <div className="min-h-dvh antialiased">
        <div className="mx-auto w-full max-w-[1100px] border-border/70 md:border-x">
          {/* Header */}
          <header className="flex h-16 items-center justify-between px-5 md:px-10">
            <a href="/" className="flex items-center gap-2.5">
              <img
                src="/app-icon.png"
                alt=""
                className="size-8 rounded-[6px]"
              />
              <span className="text-[15px] font-semibold tracking-tight">
                Waku
              </span>
            </a>
            <div className="flex items-center gap-5">
              <a
                href="https://github.com/egoist/waku"
                target="_blank"
                rel="noreferrer"
                aria-label="GitHub"
                className="rounded-full text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60"
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 24 24"
                  className='size-6'
                >
                  <path
                    fill="currentColor"
                    d="M12 2A10 10 0 0 0 2 12c0 4.42 2.87 8.17 6.84 9.5c.5.08.66-.23.66-.5v-1.69c-2.77.6-3.36-1.34-3.36-1.34c-.46-1.16-1.11-1.47-1.11-1.47c-.91-.62.07-.6.07-.6c1 .07 1.53 1.03 1.53 1.03c.87 1.52 2.34 1.07 2.91.83c.09-.65.35-1.09.63-1.34c-2.22-.25-4.55-1.11-4.55-4.92c0-1.11.38-2 1.03-2.71c-.1-.25-.45-1.29.1-2.64c0 0 .84-.27 2.75 1.02c.79-.22 1.65-.33 2.5-.33s1.71.11 2.5.33c1.91-1.29 2.75-1.02 2.75-1.02c.55 1.35.2 2.39.1 2.64c.65.71 1.03 1.6 1.03 2.71c0 3.82-2.34 4.66-4.57 4.91c.36.31.69.92.69 1.85V21c0 .27.16.59.67.5C19.14 20.16 22 16.42 22 12A10 10 0 0 0 12 2"
                  />
                </svg>
              </a>
              <DownloadMenu
                downloadUrl={downloadUrl}
                version={release?.version}
                size="sm"
                align="end"
              />
            </div>
          </header>

          <main>
            {/* Hero */}
            <section className="px-5 pt-14 pb-14 md:px-10 md:pt-24">
              <div className="mb-7 inline-flex items-center gap-2 rounded-full border px-3 py-1 text-xs text-muted-foreground">
                <span className="flex size-3.5 items-center justify-center rounded-[3px] bg-[#f26522] text-[10px] font-bold text-white">
                  Y
                </span>
                Not backed by Y Combinator
              </div>
              <h1 className="max-w-4xl text-4xl font-semibold tracking-[-0.03em] text-balance md:text-[3.4rem] md:leading-[1.04]">
                One native app for all your coding agents.
              </h1>
              <p className="mt-5 max-w-[36rem] text-[17px] leading-relaxed text-pretty text-muted-foreground">
                Waku drives the agent CLIs you already have — sessions,
                transcripts, tool activity, and checkpoints in one fast
                graphite window, entirely on your machine.
              </p>
              <div className="mt-8 flex flex-wrap items-center gap-x-5 gap-y-3">
                <DownloadMenu
                  downloadUrl={downloadUrl}
                  version={release?.version}
                  size="lg"
                  className="h-10 px-4"
                  align="start"
                  showIcon
                />
                {release && (
                  <span className="font-mono text-xs text-muted-foreground">
                    v{release.version}
                  </span>
                )}
              </div>

              {/* Providers */}
              <div className="mt-16">
                <SectionLabel>Drives the agents you already use</SectionLabel>
                <div className="mt-4 flex flex-wrap items-center gap-x-7 gap-y-4">
                  {PROVIDERS.map((p) => (
                    <Tooltip key={p.slug}>
                      <TooltipTrigger
                        render={
                          <button
                            type="button"
                            aria-label={p.label}
                            className="cursor-default rounded-sm text-muted-foreground/70 transition-colors outline-none hover:text-foreground focus-visible:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60"
                          />
                        }
                      >
                        <span
                          className="provider-mark size-[22px]"
                          style={{
                            maskImage: `url(/providers/${p.slug}.svg)`,
                            WebkitMaskImage: `url(/providers/${p.slug}.svg)`,
                          }}
                        />
                      </TooltipTrigger>
                      <TooltipContent>{p.label}</TooltipContent>
                    </Tooltip>
                  ))}
                </div>
              </div>
            </section>

            {/* Product */}
            <section>
              <picture>
                <source
                  media="(prefers-color-scheme: dark)"
                  srcSet="/app-screenshot-dark.png"
                />
                <img
                  src="/app-screenshot-light.png"
                  alt="Waku showing a coding-agent session"
                  width={2266}
                  height={1752}
                  className="block h-auto w-full"
                />
              </picture>
            </section>

            {/* Features */}
            <section className="border-t">
              <div className="px-5 pt-14 md:px-10">
                <SectionLabel>Why native</SectionLabel>
              </div>
              <div className="mt-8 grid grid-cols-1 gap-px border-t bg-border/70 sm:grid-cols-2 lg:grid-cols-3">
                {FEATURES.map((f) => (
                  <div key={f.title} className="bg-background p-6 md:p-8">
                    <div className="flex items-center gap-2.5">
                      <f.icon className="size-4 text-muted-foreground" />
                      <h3 className="text-sm font-medium">{f.title}</h3>
                    </div>
                    <p className="mt-2.5 text-sm leading-relaxed text-muted-foreground">
                      {f.body}
                    </p>
                  </div>
                ))}
              </div>
            </section>

            {/* Download */}
            <section id="download" className="border-t px-5 py-16 md:px-10 md:py-20">
              <SectionLabel>Download</SectionLabel>
              <h2 className="mt-3 text-2xl font-semibold tracking-tight">
                Get Waku
              </h2>
              <div className="mt-6 flex flex-wrap items-center gap-x-5 gap-y-3">
                <DownloadMenu
                  downloadUrl={downloadUrl}
                  version={release?.version}
                  size="lg"
                  className="h-10 px-4"
                  align="start"
                  showIcon
                />
                {release && (
                  <span className="font-mono text-xs text-muted-foreground">
                    v{release.version}
                  </span>
                )}
              </div>
            </section>

            {/* FAQ */}
            <section className="border-t px-5 py-16 md:px-10">
              <SectionLabel>Questions</SectionLabel>
              <Accordion className="mt-6 max-w-2xl">
                {FAQ.map((item) => (
                  <AccordionItem key={item.q} value={item.q}>
                    <AccordionTrigger className="text-[15px]">
                      {item.q}
                    </AccordionTrigger>
                    <AccordionContent className="max-w-[38rem] text-muted-foreground">
                      {item.a}
                    </AccordionContent>
                  </AccordionItem>
                ))}
              </Accordion>
            </section>
          </main>

          {/* Footer */}
          <footer className="flex items-center gap-2 border-t px-5 py-10 text-xs text-muted-foreground md:px-10">
            <img
              src="/app-icon.png"
              alt=""
              className="size-4 rounded-[4px] opacity-80 grayscale"
            />
            <span>© 2026 Waku</span>
          </footer>
        </div>
      </div>
    </TooltipProvider>
  )
}
