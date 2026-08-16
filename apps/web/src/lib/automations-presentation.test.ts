import { describe, expect, test } from 'bun:test'
import type { Schedule } from '@waku/client'
import {
  automationRunTimeLabel,
  automationScheduleSummary,
  automationTimeLabel,
  type AutomationTranslator,
} from './automations-presentation'

const t: AutomationTranslator = (key, params = {}) => (
  `${key}:${Object.entries(params).map(([name, value]) => `${name}=${value}`).join('|')}`
)

describe('automation presentation', () => {
  test.each([
    [{ kind: 'manual' }, 'automations.summary_manual:'],
    [{ kind: 'hourly', minute: 5 }, 'automations.summary_hourly:minute=05'],
    [
      { kind: 'daily', time: { hour: 9, minute: 7 } },
      'automations.summary_daily:time=09:07',
    ],
    [
      {
        kind: 'weekly',
        time: { hour: 18, minute: 30 },
        weekdays: ['monday', 'friday'],
      },
      'automations.summary_weekly:days=automations.weekday_mon:, automations.weekday_fri:|time=18:30',
    ],
    [
      { kind: 'monthly', time: { hour: 0, minute: 0 }, days: [1, 15, 31] },
      'automations.summary_monthly:days=1, 15, 31|time=00:00',
    ],
  ] satisfies Array<[Schedule, string]>)('summarizes %s schedules', (schedule, expected) => {
    expect(automationScheduleSummary(schedule, t)).toBe(expected)
  })

  test('formats time and run timestamps for display', () => {
    expect(automationTimeLabel({ hour: 3, minute: 4 })).toBe('03:04')
    expect(automationRunTimeLabel(0, 'en-US')).toMatch(/1970/)
  })
})
