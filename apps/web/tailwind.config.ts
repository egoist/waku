import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { iconsPlugin } from '@egoist/tailwindcss-icons'

const iconDirectory = fileURLToPath(new URL('../../assets/icons/', import.meta.url))

const icons = Object.fromEntries(
  [
    ...readdirSync(iconDirectory)
      .filter((file) => file.endsWith('.svg'))
      .map((file) => [file.slice(0, -4), readIcon(file)] as const),
    ...readdirSync(`${iconDirectory}/file-types`)
      .filter((file) => file.endsWith('.svg'))
      .map((file) => [`file-type-${file.slice(0, -4)}`, readIcon(`file-types/${file}`)] as const),
  ],
)

function readIcon(file: string) {
  const source = readFileSync(`${iconDirectory}/${file}`, 'utf8')
  const svg = source.match(/<svg\b([^>]*)>([\s\S]*)<\/svg>/i)
  if (!svg) throw new Error(`Invalid Waku icon: ${file}`)

  const viewBox = svg[1]!.match(/viewBox="([^"]+)"/i)?.[1]
    ?.trim()
    .split(/\s+/)
    .map(Number) ?? [0, 0, 24, 24]
  const [left = 0, top = 0, width = 24, height = 24] = viewBox
  const inherited = [...svg[1]!.matchAll(/\s(fill|stroke|stroke-width|stroke-linecap|stroke-linejoin|fill-rule|clip-rule)="([^"]+)"/gi)]
    .map((match) => `${match[1]}="${match[2]}"`)
    .join(' ')
  const body = (inherited ? `<g ${inherited}>${svg[2]}</g>` : svg[2]!)
    .replace(/#000(?:000)?\b/gi, 'currentColor')

  return {
    body,
    left,
    top,
    width,
    height,
  }
}

export default {
  plugins: [
    iconsPlugin({
      collections: {
        waku: {
          prefix: 'waku',
          icons,
        },
      },
      extraProperties: {
        display: 'inline-block',
      },
    }),
  ],
}
