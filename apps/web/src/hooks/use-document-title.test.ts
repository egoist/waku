import { describe, expect, test } from 'bun:test'
import {
  formatDocumentTitle,
  WAKU_DOCUMENT_TITLE,
} from './use-document-title'

describe('formatDocumentTitle', () => {
  test('uses the product title without a section', () => {
    expect(formatDocumentTitle()).toBe(WAKU_DOCUMENT_TITLE)
    expect(formatDocumentTitle('   ')).toBe(WAKU_DOCUMENT_TITLE)
  })

  test('identifies the current browser surface', () => {
    expect(formatDocumentTitle('New Task')).toBe('New Task — Waku Web')
    expect(formatDocumentTitle('  General  ')).toBe('General — Waku Web')
  })

  test('does not duplicate the product title', () => {
    expect(formatDocumentTitle(WAKU_DOCUMENT_TITLE)).toBe(WAKU_DOCUMENT_TITLE)
  })
})
