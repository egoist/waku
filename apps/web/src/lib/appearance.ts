export type ThemeChoice = 'system' | 'light' | 'dark'

export function readThemeChoice(storage: Pick<Storage, 'getItem'> | null): ThemeChoice {
  const stored = storage?.getItem('waku.theme')
  return stored === 'light' || stored === 'dark' ? stored : 'system'
}

export function resolvedTheme(
  choice: ThemeChoice,
  systemPrefersDark: boolean,
): Exclude<ThemeChoice, 'system'> {
  if (choice === 'system') return systemPrefersDark ? 'dark' : 'light'
  return choice
}

export function applyThemeChoice(
  root: Pick<HTMLElement, 'classList'>,
  choice: ThemeChoice,
  systemPrefersDark: boolean,
) {
  const resolved = resolvedTheme(choice, systemPrefersDark)
  root.classList.toggle('dark', resolved === 'dark')
  root.classList.toggle('light', resolved === 'light')
}
