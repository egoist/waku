//! Provider fallback choices used before daemon-side discovery completes.

use crate::model::{ProviderAgentPreset, ProviderKind, ProviderModel, ProviderModelOption};

pub fn fallback_models(provider: ProviderKind) -> Vec<ProviderModel> {
    match provider {
        ProviderKind::Amp => [
            ProviderModel::new("low", tr!("model_option.low")),
            ProviderModel::new("medium", tr!("model_option.medium")).default(),
            ProviderModel::new("high", tr!("model_option.high")),
            ProviderModel::new("ultra", tr!("model_option.ultra")),
        ]
        .into_iter()
        .map(|model| {
            model.service_tiers(
                [ProviderModelOption::new("fast", tr!("model_option.fast"))
                    .description(tr!("model_option.amp_fast_description"))],
                "default",
            )
        })
        .collect(),
        ProviderKind::Codex => [
            ProviderModel::new("gpt-5.6-sol", "GPT-5.6-Sol").default(),
            ProviderModel::new("gpt-5.6-terra", "GPT-5.6-Terra"),
            ProviderModel::new("gpt-5.6-luna", "GPT-5.6-Luna"),
            ProviderModel::new("gpt-5.5", "GPT-5.5"),
            ProviderModel::new("gpt-5.4", "GPT-5.4"),
        ]
        .into_iter()
        .map(|model| {
            model
                .reasoning(
                    reasoning_options(["low", "medium", "high", "xhigh"]),
                    "medium",
                )
                .service_tiers(
                    [ProviderModelOption::new("fast", tr!("model_option.fast"))
                        .description(tr!("model_option.fast_description"))],
                    "default",
                )
        })
        .collect(),
        ProviderKind::Claude => vec![
            claude_long_context(claude_ultracode_model("claude-fable-5", "Claude Fable 5")),
            claude_long_context(claude_ultracode_model("claude-opus-5", "Claude Opus 5")),
            claude_long_context(claude_ultracode_model("claude-opus-4-8", "Claude Opus 4.8")),
            claude_long_context(claude_ultracode_model("claude-opus-4-7", "Claude Opus 4.7")),
            claude_long_context(claude_reasoning_model("claude-opus-4-6", "Claude Opus 4.6")),
            claude_reasoning_model("claude-opus-4-5", "Claude Opus 4.5"),
            claude_long_context(claude_ultracode_model("claude-sonnet-5", "Claude Sonnet 5"))
                .default(),
            claude_long_context(claude_reasoning_model(
                "claude-sonnet-4-6",
                "Claude Sonnet 4.6",
            )),
            ProviderModel::new("claude-haiku-4-5", "Claude Haiku 4.5"),
        ],
        ProviderKind::Cursor => {
            vec![ProviderModel::new("auto", tr!("model_option.auto")).default()]
        }
        ProviderKind::DeepSeek | ProviderKind::OpenCode | ProviderKind::Pi => Vec::new(),
        ProviderKind::Grok => vec![ProviderModel::new("grok-build", "Grok Build").default()],
    }
}

pub fn fallback_agent_presets(provider: ProviderKind) -> Vec<ProviderAgentPreset> {
    if provider != ProviderKind::DeepSeek {
        return Vec::new();
    }
    vec![
        ProviderAgentPreset::new("standard", tr!("agent_preset.standard"))
            .description(tr!("agent_preset.standard_description"))
            .default(),
        ProviderAgentPreset::new("code", tr!("agent_preset.code"))
            .description(tr!("agent_preset.code_description")),
        ProviderAgentPreset::new("minimal", tr!("agent_preset.minimal"))
            .description(tr!("agent_preset.minimal_description")),
        ProviderAgentPreset::new("cordis", tr!("agent_preset.creator"))
            .description(tr!("agent_preset.creator_description")),
    ]
}

fn reasoning_effort_label(effort: &str) -> String {
    match effort {
        "none" => tr!("model_option.none"),
        "minimal" => tr!("model_option.minimal"),
        "low" => tr!("model_option.low"),
        "medium" => tr!("model_option.medium"),
        "high" => tr!("model_option.high"),
        "xhigh" => tr!("model_option.extra_high"),
        "max" => tr!("model_option.max"),
        "ultra" => tr!("model_option.ultra"),
        "ultracode" => tr!("model_option.ultracode"),
        other => other.replace(['-', '_'], " "),
    }
}

fn reasoning_options<const N: usize>(efforts: [&str; N]) -> Vec<ProviderModelOption> {
    efforts
        .into_iter()
        .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort)))
        .collect()
}

fn claude_reasoning_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel::new(id, name).reasoning(
        reasoning_options(["low", "medium", "high", "xhigh", "max"]),
        "high",
    )
}

/// Mirrors the daemon catalog: `ultracode` resolves to xhigh plus standing
/// dynamic-workflow orchestration, so it is only offered on xhigh-capable
/// models.
fn claude_ultracode_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel::new(id, name).reasoning(
        reasoning_options(["low", "medium", "high", "xhigh", "max", "ultracode"]),
        "high",
    )
}

/// Mirrors the daemon catalog: the 1M window is opt-in behind a `[1m]` model-id
/// suffix the CLI refuses on its older models.
fn claude_long_context(model: ProviderModel) -> ProviderModel {
    model.context_windows(
        [
            ProviderModelOption::new("200k", tr!("model_option.context_200k")),
            ProviderModelOption::new("1m", tr!("model_option.context_1m")),
        ],
        "200k",
    )
}
