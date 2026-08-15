import type { ProviderKind } from '@waku/client'

export const WAKU_ICONS = {
  alert: 'i-waku-alert',
  appearance: 'i-waku-appearance',
  arrowDown: 'i-waku-arrow-down',
  arrowLeft: 'i-waku-arrow-left',
  arrowRight: 'i-waku-arrow-right',
  arrowUp: 'i-waku-arrow-up',
  arrowUpRight: 'i-waku-arrow-up-right',
  bot: 'i-waku-bot',
  chartColumn: 'i-waku-chart-column',
  check: 'i-waku-check',
  chevronDown: 'i-waku-chevron-down',
  chevronRight: 'i-waku-chevron-right',
  cloudUpload: 'i-waku-cloud-upload',
  compose: 'i-waku-compose',
  copy: 'i-waku-copy',
  eye: 'i-waku-eye',
  eyeOff: 'i-waku-eye-off',
  file: 'i-waku-file',
  fileDiff: 'i-waku-file-diff',
  folder: 'i-waku-folder',
  folderNew: 'i-waku-folder-new',
  fork: 'i-waku-fork',
  gauge: 'i-waku-gauge',
  gitBranch: 'i-waku-git-branch',
  gitCommitHorizontal: 'i-waku-git-commit-horizontal',
  globe: 'i-waku-globe',
  github: 'i-waku-github',
  info: 'i-waku-info',
  laptop: 'i-waku-laptop',
  list: 'i-waku-list',
  loaderCircle: 'i-waku-loader-circle',
  lock: 'i-waku-lock',
  lockOpen: 'i-waku-lock-open',
  package: 'i-waku-package',
  paperclip: 'i-waku-paperclip',
  panelLeft: 'i-waku-panel-left',
  panelRight: 'i-waku-panel-right',
  pencil: 'i-waku-pencil',
  plus: 'i-waku-plus',
  rotateCw: 'i-waku-rotate-cw',
  rewind: 'i-waku-rewind',
  search: 'i-waku-search',
  server: 'i-waku-server',
  settings: 'i-waku-settings',
  sparkle: 'i-waku-sparkle',
  star: 'i-waku-star',
  starFilled: 'i-waku-star-filled',
  stop: 'i-waku-stop',
  stopFilled: 'i-waku-stop-filled',
  terminal: 'i-waku-terminal',
  terminalSquare: 'i-waku-terminal-square',
  trash: 'i-waku-trash',
  wrench: 'i-waku-wrench',
  x: 'i-waku-x',
  zap: 'i-waku-zap',
} as const

export type WakuIconName = keyof typeof WAKU_ICONS

export function WakuIcon({
  name,
  className,
  label,
}: {
  name: WakuIconName
  className?: string
  label?: string
}) {
  return (
    <span
      aria-hidden={label ? undefined : true}
      aria-label={label}
      className={`inline-grid size-4 shrink-0 place-items-center ${className ?? ''}`}
      role={label ? 'img' : undefined}
    >
      <span
        aria-hidden="true"
        className={WAKU_ICONS[name]}
        style={{ width: '100%', height: '100%' }}
      />
    </span>
  )
}

const FILE_TYPE_ICONS = {
  angular: 'i-waku-file-type-angular',
  astro: 'i-waku-file-type-astro',
  audio: 'i-waku-file-type-audio',
  babel: 'i-waku-file-type-babel',
  biome: 'i-waku-file-type-biome',
  bun: 'i-waku-file-type-bun',
  c: 'i-waku-file-type-c',
  certificate: 'i-waku-file-type-certificate',
  clojure: 'i-waku-file-type-clojure',
  cmake: 'i-waku-file-type-cmake',
  coffee: 'i-waku-file-type-coffee',
  console: 'i-waku-file-type-console',
  cpp: 'i-waku-file-type-cpp',
  crystal: 'i-waku-file-type-crystal',
  csharp: 'i-waku-file-type-csharp',
  css: 'i-waku-file-type-css',
  dart: 'i-waku-file-type-dart',
  database: 'i-waku-file-type-database',
  deno: 'i-waku-file-type-deno',
  diff: 'i-waku-file-type-diff',
  docker: 'i-waku-file-type-docker',
  editorconfig: 'i-waku-file-type-editorconfig',
  elixir: 'i-waku-file-type-elixir',
  elm: 'i-waku-file-type-elm',
  erlang: 'i-waku-file-type-erlang',
  eslint: 'i-waku-file-type-eslint',
  exe: 'i-waku-file-type-exe',
  file: 'i-waku-file-type-file',
  firebase: 'i-waku-file-type-firebase',
  git: 'i-waku-file-type-git',
  gitlab: 'i-waku-file-type-gitlab',
  go: 'i-waku-file-type-go',
  gradle: 'i-waku-file-type-gradle',
  graphql: 'i-waku-file-type-graphql',
  haskell: 'i-waku-file-type-haskell',
  haxe: 'i-waku-file-type-haxe',
  helm: 'i-waku-file-type-helm',
  html: 'i-waku-file-type-html',
  image: 'i-waku-file-type-image',
  java: 'i-waku-file-type-java',
  javascript: 'i-waku-file-type-javascript',
  jinja: 'i-waku-file-type-jinja',
  json: 'i-waku-file-type-json',
  julia: 'i-waku-file-type-julia',
  kotlin: 'i-waku-file-type-kotlin',
  kubernetes: 'i-waku-file-type-kubernetes',
  lock: 'i-waku-file-type-lock',
  lua: 'i-waku-file-type-lua',
  makefile: 'i-waku-file-type-makefile',
  markdown: 'i-waku-file-type-markdown',
  nest: 'i-waku-file-type-nest',
  next: 'i-waku-file-type-next',
  nginx: 'i-waku-file-type-nginx',
  nix: 'i-waku-file-type-nix',
  nodejs: 'i-waku-file-type-nodejs',
  npm: 'i-waku-file-type-npm',
  nuxt: 'i-waku-file-type-nuxt',
  ocaml: 'i-waku-file-type-ocaml',
  pdf: 'i-waku-file-type-pdf',
  perl: 'i-waku-file-type-perl',
  php: 'i-waku-file-type-php',
  pnpm: 'i-waku-file-type-pnpm',
  powershell: 'i-waku-file-type-powershell',
  prettier: 'i-waku-file-type-prettier',
  prisma: 'i-waku-file-type-prisma',
  proto: 'i-waku-file-type-proto',
  pug: 'i-waku-file-type-pug',
  python: 'i-waku-file-type-python',
  react: 'i-waku-file-type-react',
  readme: 'i-waku-file-type-readme',
  rollup: 'i-waku-file-type-rollup',
  ruby: 'i-waku-file-type-ruby',
  rust: 'i-waku-file-type-rust',
  sass: 'i-waku-file-type-sass',
  scala: 'i-waku-file-type-scala',
  settings: 'i-waku-file-type-settings',
  solidity: 'i-waku-file-type-solidity',
  storybook: 'i-waku-file-type-storybook',
  stylelint: 'i-waku-file-type-stylelint',
  supabase: 'i-waku-file-type-supabase',
  svelte: 'i-waku-file-type-svelte',
  svg: 'i-waku-file-type-svg',
  swift: 'i-waku-file-type-swift',
  tailwindcss: 'i-waku-file-type-tailwindcss',
  terraform: 'i-waku-file-type-terraform',
  tex: 'i-waku-file-type-tex',
  turborepo: 'i-waku-file-type-turborepo',
  typescript: 'i-waku-file-type-typescript',
  video: 'i-waku-file-type-video',
  vite: 'i-waku-file-type-vite',
  vitest: 'i-waku-file-type-vitest',
  vue: 'i-waku-file-type-vue',
  webassembly: 'i-waku-file-type-webassembly',
  webpack: 'i-waku-file-type-webpack',
  xaml: 'i-waku-file-type-xaml',
  xml: 'i-waku-file-type-xml',
  yaml: 'i-waku-file-type-yaml',
  yarn: 'i-waku-file-type-yarn',
  zig: 'i-waku-file-type-zig',
  zip: 'i-waku-file-type-zip',
} as const

type FileTypeIconName = keyof typeof FILE_TYPE_ICONS

export function FileTypeIcon({
  path,
  className,
}: {
  path: string
  className?: string
}) {
  const name = fileTypeIconName(path)
  return (
    <span
      aria-hidden="true"
      className={`inline-grid size-4 shrink-0 place-items-center ${className ?? ''}`}
    >
      <span className={FILE_TYPE_ICONS[name]} style={{ width: '100%', height: '100%' }} />
    </span>
  )
}

function fileTypeIconName(path: string): FileTypeIconName {
  const name = path.split(/[\\/]/).at(-1)?.toLocaleLowerCase() ?? path.toLocaleLowerCase()
  if (name.startsWith('readme')) return 'readme'
  if (/^(license|licence|copying)/.test(name)) return 'certificate'
  if (name.startsWith('dockerfile') || name.startsWith('compose.')) return 'docker'
  if (name === 'cmakelists.txt' || name.startsWith('cmake.')) return 'cmake'
  if (name === 'makefile' || name.startsWith('makefile.') || name === 'justfile') return 'makefile'
  if (['cargo.toml', 'cargo.lock', 'rust-toolchain.toml'].includes(name)) return 'rust'
  if (['go.mod', 'go.sum', 'go.work'].includes(name)) return 'go'
  if (name === 'pyproject.toml' || name === 'pipfile' || name.startsWith('requirements')) return 'python'
  if (['bun.lock', 'bun.lockb', 'bunfig.toml'].includes(name)) return 'bun'
  if (name.startsWith('pnpm-') || name === '.pnpmfile.cjs') return 'pnpm'
  if (name === 'yarn.lock' || name.startsWith('.yarnrc')) return 'yarn'
  if (name === 'package.json') return 'nodejs'
  if (name === 'package-lock.json') return 'npm'
  if (name === 'tsconfig.json' || name.startsWith('tsconfig.')) return 'typescript'
  if (name === 'jsconfig.json' || name.startsWith('jsconfig.')) return 'javascript'
  if (['.gitignore', '.gitattributes', '.gitmodules', '.gitconfig'].includes(name)) return 'git'
  if (name === '.editorconfig') return 'editorconfig'
  if (name.startsWith('.env')) return 'settings'
  if (name.startsWith('.prettier') || name.startsWith('prettier.config.')) return 'prettier'
  if (name.startsWith('.eslint') || name.startsWith('eslint.config.')) return 'eslint'
  if (name.startsWith('biome.json')) return 'biome'
  if (name.startsWith('.babel') || name.startsWith('babel.config.')) return 'babel'
  if (name.startsWith('.stylelint') || name.startsWith('stylelint.config.')) return 'stylelint'
  if (name.startsWith('vite.config.')) return 'vite'
  if (name.startsWith('vitest.config.') || name.startsWith('vitest.workspace.')) return 'vitest'
  if (name.startsWith('webpack.')) return 'webpack'
  if (name.startsWith('rollup.config.')) return 'rollup'
  if (name.startsWith('next.config.') || name === 'next-env.d.ts') return 'next'
  if (name.startsWith('nuxt.config.') || name === '.nuxtrc') return 'nuxt'
  if (name.startsWith('astro.config.')) return 'astro'
  if (name === 'angular.json' || name.endsWith('.component.ts')) return 'angular'
  if (name === 'nest-cli.json') return 'nest'
  if (name.startsWith('tailwind.config.')) return 'tailwindcss'
  if (name.startsWith('svelte.config.')) return 'svelte'
  if (name.startsWith('vue.config.')) return 'vue'
  if (name === 'firebase.json' || name === '.firebaserc') return 'firebase'
  if (name === 'supabase.toml') return 'supabase'
  if (name.startsWith('prisma.config.')) return 'prisma'
  if (name === 'turbo.json') return 'turborepo'
  if (name.startsWith('deno.json') || name === 'deno.lock') return 'deno'
  if (name === '.gitlab-ci.yml' || name === '.gitlab-ci.yaml') return 'gitlab'
  if (name === 'kustomization.yaml' || name === 'kustomization.yml') return 'kubernetes'
  if (name === 'chart.yaml' || name === 'values.yaml') return 'helm'
  if (name === 'nginx.conf') return 'nginx'
  if (name === '.nvmrc' || name === '.node-version') return 'nodejs'
  if (['build.gradle', 'settings.gradle', 'gradlew', 'gradlew.bat'].includes(name)) return 'gradle'
  if (name.includes('.stories.') || name.includes('.story.')) return 'storybook'
  if (name === 'gemfile' || name === 'gemfile.lock') return 'ruby'
  if (name === 'pom.xml') return 'java'

  const extension = name.includes('.') ? name.split('.').at(-1) ?? '' : ''
  if (extension === 'rs') return 'rust'
  if (['js', 'mjs', 'cjs'].includes(extension)) return 'javascript'
  if (['ts', 'mts', 'cts'].includes(extension)) return 'typescript'
  if (['jsx', 'tsx'].includes(extension)) return 'react'
  if (['py', 'pyi', 'pyw'].includes(extension)) return 'python'
  if (extension === 'go') return 'go'
  if (['c', 'h', 'm'].includes(extension)) return 'c'
  if (['cc', 'cpp', 'cxx', 'hh', 'hpp', 'hxx', 'mm'].includes(extension)) return 'cpp'
  if (extension === 'cs') return 'csharp'
  if (extension === 'swift') return 'swift'
  if (['kt', 'kts'].includes(extension)) return 'kotlin'
  if (['java', 'class'].includes(extension)) return 'java'
  if (extension === 'rb') return 'ruby'
  if (extension === 'php') return 'php'
  if (['html', 'htm'].includes(extension)) return 'html'
  if (['css', 'less'].includes(extension)) return 'css'
  if (['scss', 'sass'].includes(extension)) return 'sass'
  if (['json', 'jsonc', 'jsonl'].includes(extension)) return 'json'
  if (['yaml', 'yml'].includes(extension)) return 'yaml'
  if (['toml', 'ini', 'cfg', 'conf', 'config'].includes(extension)) return 'settings'
  if (['xml', 'xsl', 'plist'].includes(extension)) return 'xml'
  if (['md', 'mdx', 'markdown'].includes(extension)) return 'markdown'
  if (['sh', 'bash', 'zsh', 'fish'].includes(extension)) return 'console'
  if (['ps1', 'psm1'].includes(extension)) return 'powershell'
  if (['sql', 'db', 'sqlite', 'sqlite3', 'csv', 'xls', 'xlsx'].includes(extension)) return 'database'
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'ico', 'tiff'].includes(extension)) return 'image'
  if (extension === 'svg') return 'svg'
  if (extension === 'pdf') return 'pdf'
  if (['mp3', 'wav', 'flac', 'ogg', 'm4a'].includes(extension)) return 'audio'
  if (['mp4', 'mov', 'avi', 'webm', 'mkv'].includes(extension)) return 'video'
  if (['zip', 'gz', 'tgz', 'bz2', 'xz', '7z', 'rar', 'tar', 'jar'].includes(extension)) return 'zip'
  if (['wasm', 'wat'].includes(extension)) return 'webassembly'
  if (['svelte', 'vue', 'lua', 'dart', 'astro', 'prisma', 'xaml', 'zig', 'nix', 'proto'].includes(extension)) return extension as FileTypeIconName
  if (['tf', 'tfvars'].includes(extension)) return 'terraform'
  if (['graphql', 'gql'].includes(extension)) return 'graphql'
  if (['coffee', 'cson'].includes(extension)) return 'coffee'
  if (extension === 'cr') return 'crystal'
  if (['ex', 'exs'].includes(extension)) return 'elixir'
  if (extension === 'elm') return 'elm'
  if (['erl', 'hrl'].includes(extension)) return 'erlang'
  if (['clj', 'cljs', 'cljc', 'edn'].includes(extension)) return 'clojure'
  if (['hs', 'lhs'].includes(extension)) return 'haskell'
  if (['hx', 'hxml'].includes(extension)) return 'haxe'
  if (['jinja', 'jinja2', 'j2'].includes(extension)) return 'jinja'
  if (extension === 'jl') return 'julia'
  if (['ml', 'mli'].includes(extension)) return 'ocaml'
  if (['pl', 'pm'].includes(extension)) return 'perl'
  if (['pug', 'jade'].includes(extension)) return 'pug'
  if (['scala', 'sbt', 'sc'].includes(extension)) return 'scala'
  if (extension === 'sol') return 'solidity'
  if (['tex', 'sty', 'cls'].includes(extension)) return 'tex'
  if (['diff', 'patch'].includes(extension)) return 'diff'
  if (['exe', 'dll', 'so', 'dylib'].includes(extension)) return 'exe'
  if (extension === 'lock') return 'lock'
  return 'file'
}

const PROVIDER_ICONS: Record<ProviderKind, string> = {
  amp: 'i-waku-provider-amp',
  claude: 'i-waku-provider-claude',
  codex: 'i-waku-provider-openai',
  cursor: 'i-waku-provider-cursor',
  deepSeek: 'i-waku-provider-deepseek',
  openCode: 'i-waku-provider-opencode',
  grok: 'i-waku-provider-grok',
  pi: 'i-waku-provider-pi',
}

export const PROVIDERS: Array<{
  id: ProviderKind
  name: string
  shortName: string
  command: string
}> = [
  { id: 'amp', name: 'Amp', shortName: 'Amp', command: 'amp' },
  { id: 'claude', name: 'Claude Code', shortName: 'Claude', command: 'claude' },
  { id: 'codex', name: 'Codex CLI', shortName: 'Codex', command: 'codex' },
  { id: 'cursor', name: 'Cursor CLI', shortName: 'Cursor', command: 'cursor-agent' },
  { id: 'deepSeek', name: 'DeepSeek Harness', shortName: 'DeepSeek', command: 'dsh' },
  { id: 'openCode', name: 'OpenCode', shortName: 'OpenCode', command: 'opencode' },
  { id: 'grok', name: 'Grok Build', shortName: 'Grok', command: 'grok' },
  { id: 'pi', name: 'Pi', shortName: 'Pi', command: 'pi' },
]

export function providerMeta(provider: ProviderKind) {
  return PROVIDERS.find((candidate) => candidate.id === provider) ?? PROVIDERS[2]!
}

export function ProviderIcon({
  provider,
  className,
  label,
}: {
  provider: ProviderKind
  className?: string
  label?: string
}) {
  return (
    <span
      aria-hidden={label ? undefined : true}
      aria-label={label}
      className={`inline-grid size-4 shrink-0 place-items-center ${providerColor(provider)} ${className ?? ''}`}
      role={label ? 'img' : undefined}
    >
      <span
        aria-hidden="true"
        className={PROVIDER_ICONS[provider]}
        style={{ width: '100%', height: '100%' }}
      />
    </span>
  )
}

function providerColor(provider: ProviderKind) {
  if (provider === 'amp') return 'text-[#f34e3f]'
  if (provider === 'claude') return 'text-[#d97757]'
  if (provider === 'deepSeek') return 'text-[#4d6bfe]'
  return 'text-[#34363b] dark:text-[#f3f3f3]'
}
