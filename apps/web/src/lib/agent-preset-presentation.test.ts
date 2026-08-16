import { describe, expect, test } from 'bun:test'
import type { ProviderAgentPreset } from '@waku/client'
import {
  agentPresetDescription,
  agentPresetIdLabel,
  agentPresetLabel,
} from './agent-preset-presentation'
import { translate } from './i18n'

const english = (key: string) => translate('en', key)

describe('DeepSeek agent preset presentation', () => {
  test('localizes shipped presets instead of displaying the daemon host language', () => {
    const preset: ProviderAgentPreset = {
      id: 'standard',
      name: '标准模式',
      description: '主机本地化的描述',
      is_default: true,
      is_custom: false,
    }

    expect(agentPresetLabel(preset, english)).toBe('Standard mode')
    expect(agentPresetDescription(preset, english)).toStartWith('Full coding agent')
    expect(agentPresetIdLabel('standard', english)).toBe('Standard mode')
  })

  test('preserves user-authored preset metadata', () => {
    const preset: ProviderAgentPreset = {
      id: 'my-agent',
      name: 'My agent',
      description: 'My description',
      is_default: false,
      is_custom: true,
    }

    expect(agentPresetLabel(preset, english)).toBe('My agent')
    expect(agentPresetDescription(preset, english)).toBe('My description')
    expect(agentPresetIdLabel('my-agent', english)).toBe('my-agent')
  })
})
