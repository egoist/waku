import type { Schedule, TimeOfDay, Weekday } from '@waku/client'

/**
 * Translator shape used by automation presentation helpers.
 */
export type AutomationTranslator = (
  key: string,
  params?: Record<string, string | number>,
) => string

/**
 * Builds a localized, compact description of an automation schedule.
 *
 * @param schedule Schedule to describe.
 * @param t Translation function for the active locale.
 * @returns A human-readable schedule summary.
 */
export function automationScheduleSummary(
  schedule: Schedule,
  t: AutomationTranslator,
): string {
  if (schedule.kind === 'manual') return t('automations.summary_manual')
  if (schedule.kind === 'hourly') {
    return t('automations.summary_hourly', { minute: pad(schedule.minute) })
  }

  const time = automationTimeLabel(schedule.time)
  if (schedule.kind === 'daily') return t('automations.summary_daily', { time })
  if (schedule.kind === 'weekly') {
    return t('automations.summary_weekly', {
      days: schedule.weekdays.map((day) => automationWeekdayLabel(day, t)).join(', '),
      time,
    })
  }
  return t('automations.summary_monthly', {
    days: schedule.days.join(', '),
    time,
  })
}

/**
 * Formats a local automation time using a stable 24-hour clock.
 *
 * @param time Local wall-clock time.
 * @returns A zero-padded HH:mm label.
 */
export function automationTimeLabel(time: TimeOfDay): string {
  return `${pad(time.hour)}:${pad(time.minute)}`
}

/**
 * Formats a run timestamp for the active application locale.
 *
 * @param timestamp Unix timestamp in seconds.
 * @param locale Active application locale.
 * @returns A localized date and time label.
 */
export function automationRunTimeLabel(timestamp: number, locale: string): string {
  return new Date(timestamp * 1_000).toLocaleString(locale, {
    dateStyle: 'medium',
    timeStyle: 'short',
  })
}

function automationWeekdayLabel(day: Weekday, t: AutomationTranslator): string {
  return t(`automations.weekday_${day.slice(0, 3)}`)
}

function pad(value: number): string {
  return String(value).padStart(2, '0')
}
