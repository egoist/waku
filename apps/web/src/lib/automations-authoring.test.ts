import { describe, expect, test } from 'bun:test'
import type { AgentSession } from '@waku/client'
import {
  automationFromForm,
  createAutomation,
  createDeleteConfirmation,
  scheduleWithKind,
} from './automations-authoring'

describe('automation form mapping', () => {
  test('maps every authoring policy into a normalized automation', () => {
    const form = createAutomation('automation', 100)
    form.name = '  Weekly review  '
    form.prompt = '  Review the repository  '
    form.agent = {
      provider: 'claude',
      model: ' opus ',
      reasoning_effort: ' high ',
      service_tier: ' priority ',
      agent_preset: ' reviewer ',
      runtime_mode: 'ask',
      interaction_mode: 'plan',
    }
    form.project_id = 'project'
    form.workspace = { kind: 'newWorktree', baseBranch: 'main' }
    form.schedule = {
      kind: 'monthly',
      time: { hour: 25, minute: -2 },
      days: [31, 2, 2, 45],
    }
    form.overlap = 'queue'
    form.notification = { enabled: false, trigger: 'onSuccess' }
    form.enabled = false

    expect(automationFromForm(form, 200)).toMatchObject({
      id: 'automation',
      name: 'Weekly review',
      prompt: 'Review the repository',
      agent: {
        provider: 'claude',
        model: 'opus',
        reasoning_effort: 'high',
        service_tier: 'priority',
        agent_preset: 'reviewer',
        runtime_mode: 'ask',
        interaction_mode: 'plan',
      },
      project_id: 'project',
      workspace: { kind: 'newWorktree', baseBranch: 'main' },
      schedule: {
        kind: 'monthly',
        time: { hour: 23, minute: 0 },
        days: [2, 31],
      },
      overlap: 'queue',
      notification: { enabled: false, trigger: 'onSuccess' },
      enabled: false,
      created_at: 100,
      updated_at: 200,
    })
  })

  test('carries time between schedule frequencies and supplies required selections', () => {
    expect(scheduleWithKind({ kind: 'hourly', minute: 42 }, 'weekly')).toEqual({
      kind: 'weekly',
      time: { hour: 9, minute: 42 },
      weekdays: ['monday'],
    })
    expect(
      scheduleWithKind(
        {
          kind: 'monthly',
          time: { hour: 7, minute: 15 },
          days: [5],
        },
        'daily',
      ),
    ).toEqual({
      kind: 'daily',
      time: { hour: 7, minute: 15 },
    })
  })
})

describe('automation deletion confirmation', () => {
  test('defaults cascade off and reports the exact automation session count', () => {
    const sessions = [
      { originating_automation: 'automation' },
      { originating_automation: 'other' },
      { originating_automation: 'automation' },
      { originating_automation: null },
    ] as AgentSession[]

    expect(createDeleteConfirmation('automation', sessions)).toEqual({
      automationId: 'automation',
      cascadeSessions: false,
      sessionCount: 2,
    })
  })
})
