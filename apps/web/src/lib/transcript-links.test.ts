import { describe, expect, test } from 'bun:test'
import { transcriptLinkRoute } from './transcript-links'

describe('transcriptLinkRoute', () => {
  const workspace = '/Users/egoist/dev/waku'

  test('opens absolute workspace files in the project editor', () => {
    expect(transcriptLinkRoute(
      '/Users/egoist/dev/waku/src/app/right_panel.rs:1596:8',
      workspace,
    )).toEqual({ kind: 'projectFile', path: 'src/app/right_panel.rs' })
    expect(transcriptLinkRoute(
      'file:///Users/egoist/dev/waku/My%20File.rs#L12C4',
      workspace,
    )).toEqual({ kind: 'projectFile', path: 'My File.rs' })
  })

  test('does not reinterpret another daemon path or an external URL', () => {
    expect(transcriptLinkRoute(
      '/Users/egoist/dev/waku/../kero/src/app.rs:20',
      workspace,
    )).toEqual({ kind: 'remoteFile', path: '/Users/egoist/dev/kero/src/app.rs' })
    expect(transcriptLinkRoute('https://example.com/file.rs:12', workspace))
      .toEqual({ kind: 'external' })
    expect(transcriptLinkRoute('src/app.rs', workspace)).toEqual({ kind: 'external' })
  })
})
