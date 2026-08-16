/// <reference types="vite/client" />
import type { QueryClient } from '@tanstack/react-query'
import {
  HeadContent,
  Outlet,
  Scripts,
  createRootRouteWithContext,
} from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { Toaster } from 'sonner'
import { DaemonProvider } from '@/lib/daemon-context'
import { RuntimeProvider } from '@/lib/runtime-context'
import appCss from '@/styles.css?url'

const TITLE = 'Waku Web'
const DESCRIPTION =
  'Connect securely to a Waku daemon and continue your coding-agent tasks from the browser.'

export const Route = createRootRouteWithContext<{
  queryClient: QueryClient
}>()({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      {
        name: 'viewport',
        content: 'width=device-width, initial-scale=1, viewport-fit=cover',
      },
      { title: TITLE },
      { name: 'description', content: DESCRIPTION },
      { name: 'robots', content: 'noindex, nofollow' },
      {
        name: 'theme-color',
        media: '(prefers-color-scheme: light)',
        content: '#f7f7f6',
      },
      {
        name: 'theme-color',
        media: '(prefers-color-scheme: dark)',
        content: '#191a1a',
      },
    ],
    links: [{ rel: 'stylesheet', href: appCss }],
    scripts: [
      {
        children: `try{var d=document.documentElement,p=localStorage.getItem('waku.theme'),s=matchMedia('(prefers-color-scheme: dark)').matches,x=p==='dark'||p!=='light'&&s,l=localStorage.getItem('waku.language'),n=(navigator.languages&&navigator.languages[0]||navigator.language||'en').replaceAll('_','-').toLowerCase(),r=l==='zh-CN'||l==='ja'||l==='en'?l:n==='zh-cn'||n==='zh-sg'||n.startsWith('zh-hans')?'zh-CN':n==='ja'||n.startsWith('ja-')?'ja':'en';d.classList.toggle('dark',x);d.classList.toggle('light',!x);d.lang=r}catch(e){}`,
      },
    ],
  }),
  component: RootComponent,
})

function RootComponent() {
  return (
    <RootDocument>
      <Outlet />
    </RootDocument>
  )
}

function RootDocument({ children }: { children: ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <HeadContent />
      </head>
      <body>
        <DaemonProvider>
          <RuntimeProvider>{children}</RuntimeProvider>
        </DaemonProvider>
        <Toaster position="top-center" richColors closeButton />
        <Scripts />
      </body>
    </html>
  )
}
