//! Provider model and agent-preset discovery.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{ClientCapabilities, Implementation, InitializeRequest};
use agent_client_protocol::{Agent, Client, ConnectionTo, UntypedMessage};
use serde_json::{Map, Value, json};

use crate::model::{ProviderAgentPreset, ProviderKind, ProviderModel, ProviderModelOption};
use waku_protocol::model_catalog::normalize_cursor_reasoning_effort;

const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const PI_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const CURSOR_ACP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const DROID_ACP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

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
        // Cursor's full catalog is account-specific and comes from ACP
        // `cursor/list_available_models`. Auto remains the provider-owned
        // default and keeps older CLIs selectable if discovery is unavailable.
        ProviderKind::Cursor => {
            vec![ProviderModel::new("auto", tr!("model_option.auto")).default()]
        }
        // Droid's catalog is account-specific (BYOK routes and open models
        // come and go), so only the Factory-router default stands in before
        // discovery answers; `auto` is valid on every account.
        ProviderKind::Droid => {
            vec![ProviderModel::new("auto", tr!("model_option.auto")).default()]
        }
        // Harness reports its account/configuration-specific catalog from its
        // Host. An invented fallback would make unavailable routes selectable.
        ProviderKind::DeepSeek => Vec::new(),
        // Fx resolves its catalog through the user's active Gateway or
        // subscription login. An invented fallback could expose an unusable
        // route, so discovery is authoritative.
        ProviderKind::Fx => Vec::new(),
        // Discovery through the adopted v2 service is authoritative; a
        // fabricated catalog would offer models the server rejects.
        ProviderKind::OpenCode | ProviderKind::OpenCode2 => Vec::new(),
        // Grok's catalog comes from `grok models`, which includes any custom
        // model the user configured. An invented fallback would offer a model
        // the CLI rejects, so discovery is authoritative.
        ProviderKind::Grok => Vec::new(),
        // Pi, Oh My Pi, and Kimi Code all take their catalog from the user's
        // configured LLM providers. A fabricated fallback would make
        // unavailable models look selectable.
        ProviderKind::Kimi | ProviderKind::OhMyPi | ProviderKind::Pi => Vec::new(),
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

/// Discovers both ordinary models and provider-owned agent compositions in
/// one provider process. Harness serves both catalogs from the same resident
/// Host, so querying them together avoids starting it twice during detection.
pub fn discover_catalog(
    provider: ProviderKind,
    binary: &Path,
) -> (Vec<ProviderModel>, Vec<ProviderAgentPreset>) {
    let (discovered, discovered_presets) = match provider {
        // Amp exposes stable agent modes rather than a model inventory. Keep
        // the picker aligned with the modes advertised by the current CLI.
        ProviderKind::Amp => (Vec::new(), None),
        ProviderKind::Codex => (discover_codex_models(binary), None),
        ProviderKind::Claude => (discover_claude_models(binary), None),
        ProviderKind::Cursor => (discover_cursor_models(binary), None),
        ProviderKind::DeepSeek => discover_deepseek_catalog(binary),
        ProviderKind::Droid => (discover_droid_models(binary), None),
        ProviderKind::Fx => (discover_fx_models(binary), None),
        ProviderKind::OpenCode => (discover_opencode_models(binary), None),
        ProviderKind::OpenCode2 => crate::opencode2_session::discover_catalog(binary),
        ProviderKind::Grok => (discover_grok_models(binary), None),
        ProviderKind::Kimi => (discover_kimi_models(binary), None),
        ProviderKind::Pi => (discover_pi_models(binary, PiDialect::Pi), None),
        ProviderKind::OhMyPi => (discover_pi_models(binary, PiDialect::OhMyPi), None),
    };
    let models = if discovered.is_empty() {
        // A failed or empty probe keeps the last successful discovery over
        // the hardcoded catalog, so one bad CLI run can't shrink the picker.
        cached_models(provider).unwrap_or_else(|| fallback_models(provider))
    } else {
        let models = deduplicate(discovered);
        write_cached_models(provider, &models);
        models
    };
    let presets = discovered_presets.unwrap_or_else(|| fallback_agent_presets(provider));
    (models, presets)
}

/// Where a provider's last discovered catalog is cached. Debug builds keep it
/// in the checkout's gitignored `temp/` beside the debug database, so
/// development never touches the installed app's cache.
fn model_cache_path(provider: ProviderKind) -> PathBuf {
    let directory = if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("temp")
            .join("model-cache")
    } else {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(crate::identity::DATA_DIRECTORY_NAME)
            .join("models")
    };
    directory.join(format!("{}.json", provider.id()))
}

/// The catalog cached by the last successful discovery, or `None` when no run
/// has cached one or the file no longer parses. Reads the filesystem, so call
/// it from the discovery thread, never from render.
pub fn cached_models(provider: ProviderKind) -> Option<Vec<ProviderModel>> {
    read_models_file(&model_cache_path(provider))
}

fn read_models_file(path: &Path) -> Option<Vec<ProviderModel>> {
    let contents = std::fs::read(path).ok()?;
    let models = serde_json::from_slice::<Vec<ProviderModel>>(&contents).ok()?;
    (!models.is_empty()).then_some(models)
}

/// Best-effort: a cache that fails to write only costs the next launch its
/// head start.
fn write_cached_models(provider: ProviderKind, models: &[ProviderModel]) {
    let _ = write_models_file(&model_cache_path(provider), models);
}

fn write_models_file(path: &Path, models: &[ProviderModel]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a crash mid-write can't leave a torn file for the
    // next launch to trip over.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(models)?)?;
    std::fs::rename(temporary, path)
}

/// Claude Code's sessionless SDK initialization response carries the exact
/// catalog behind `/model`. Unlike a fixed Anthropic list, it reflects account
/// availability and role mappings supplied by tools such as CC Switch.
fn discover_claude_models(binary: &Path) -> Vec<ProviderModel> {
    crate::claude_metadata::initialize(binary, None, "user")
        .map(|value| parse_claude_models(&value))
        .unwrap_or_default()
}

fn parse_claude_models(value: &Value) -> Vec<ProviderModel> {
    value
        .pointer("/response/response/models")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;
            let resolved = entry
                .get("resolvedModel")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty());
            let name = entry
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .or_else(|| resolved.map(display_name_from_slug))
                .unwrap_or_else(|| display_name_from_slug(id));

            let mut model = ProviderModel::new(id, name);
            model.is_default =
                id == "default" || entry.get("isDefault").and_then(Value::as_bool) == Some(true);
            if entry.get("supportsEffort").and_then(Value::as_bool) != Some(false) {
                model.reasoning_efforts = entry
                    .get("supportedEffortLevels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|effort| !effort.is_empty())
                    .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort)))
                    .collect();
                // `ultracode` is Waku's orchestration effort. Claude accepts
                // it wherever the provider metadata says xhigh is supported.
                if model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == "xhigh")
                {
                    model.reasoning_efforts.push(ProviderModelOption::new(
                        "ultracode",
                        reasoning_effort_label("ultracode"),
                    ));
                }
                model.default_reasoning_effort = ["high", "medium"]
                    .into_iter()
                    .find(|preferred| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == *preferred)
                    })
                    .or_else(|| {
                        model
                            .reasoning_efforts
                            .first()
                            .map(|option| option.id.as_str())
                    })
                    .map(str::to_owned);
            }
            Some(model)
        })
        .collect()
}

fn discover_cursor_models(binary: &Path) -> Vec<ProviderModel> {
    let discovered = discover_cursor_models_via_acp(binary);
    if !discovered.is_empty() {
        return discovered;
    }
    let mut command = crate::command_env::command(binary);
    let command = command.arg("models");
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cursor_models(&combined)
}

/// Cursor's parameterized picker is an ACP extension: `cursor-agent models`
/// still prints exploded CLI aliases, while `cursor/list_available_models`
/// returns the base slug plus per-model config options (effort, fast, context).
fn discover_cursor_models_via_acp(binary: &Path) -> Vec<ProviderModel> {
    let Ok(cwd) = crate::acp_session::catalog_working_directory() else {
        return Vec::new();
    };
    let Ok(agent) = crate::driver::catalog_agent(ProviderKind::Cursor, binary, &cwd) else {
        return Vec::new();
    };
    let request = Client
        .builder()
        .name("waku-cursor-model-discovery")
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let mut capabilities = ClientCapabilities::new().terminal(false);
            let mut meta = Map::new();
            meta.insert("parameterizedModelPicker".to_owned(), Value::Bool(true));
            capabilities = capabilities.meta(meta);
            connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(capabilities)
                        .client_info(Implementation::new("waku", env!("CARGO_PKG_VERSION"))),
                )
                .block_task()
                .await?;
            let request = UntypedMessage::new("cursor/list_available_models", json!({}))?;
            connection.send_request(request).block_task().await
        });
    let response = smol::block_on(smol::future::race(
        async move { request.await.map_err(|_| ()) },
        async move {
            smol::Timer::after(CURSOR_ACP_DISCOVERY_TIMEOUT).await;
            Err(())
        },
    ));
    response
        .ok()
        .map(|value| parse_cursor_available_models(&value))
        .unwrap_or_default()
}

fn parse_cursor_available_models(value: &Value) -> Vec<ProviderModel> {
    value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_cursor_available_model)
        .collect()
}

fn parse_cursor_available_model(entry: &Value) -> Option<ProviderModel> {
    let value = entry
        .get("value")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| display_name_from_slug(value));
    let id = if value == "default" { "auto" } else { value };
    let mut model = ProviderModel::new(id, name);
    if id == "auto" || value == "default" {
        model = model.default();
    }
    let config_options = entry
        .get("configOptions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Some(with_cursor_config_options(model, config_options))
}

fn with_cursor_config_options(mut model: ProviderModel, options: &[Value]) -> ProviderModel {
    if let Some(effort) = find_cursor_effort_json(options) {
        let mut efforts = Vec::new();
        for (id, name) in json_select_choices(effort) {
            let id = normalize_cursor_reasoning_effort(&id);
            if id.is_empty()
                || efforts
                    .iter()
                    .any(|option: &ProviderModelOption| option.id == id)
            {
                continue;
            }
            let label = cursor_select_label(&id, &name);
            efforts.push(ProviderModelOption::new(id, label));
        }
        if !efforts.is_empty() {
            let current =
                json_current_string(effort).map(|value| normalize_cursor_reasoning_effort(&value));
            let default = current
                .filter(|id| efforts.iter().any(|option| option.id == *id))
                .or_else(|| {
                    ["high", "medium"]
                        .into_iter()
                        .find(|preferred| efforts.iter().any(|option| option.id == *preferred))
                        .map(str::to_owned)
                })
                .or_else(|| efforts.first().map(|option| option.id.clone()));
            if let Some(default) = default {
                model = model.reasoning(efforts, default);
            }
        }
    }
    if let Some(fast) = options.iter().find(|option| is_cursor_fast_json(option)) {
        let default = if json_current_bool(fast) == Some(true) {
            "fast"
        } else {
            "default"
        };
        model = model.service_tiers(
            [ProviderModelOption::new("fast", tr!("model_option.fast"))
                .description(tr!("model_option.fast_description"))],
            default,
        );
    }
    if let Some(context) = options.iter().find(|option| is_cursor_context_json(option)) {
        let windows: Vec<_> = json_select_choices(context)
            .into_iter()
            .filter(|(id, _)| !id.is_empty())
            .map(|(id, name)| {
                let label = if name.trim().is_empty() || name.contains(":icon-") {
                    id.clone()
                } else {
                    name
                };
                ProviderModelOption::new(id, label)
            })
            .collect();
        if !windows.is_empty() {
            let default = json_current_string(context)
                .filter(|id| windows.iter().any(|option| option.id == *id))
                .or_else(|| windows.first().map(|option| option.id.clone()));
            if let Some(default) = default {
                model = model.context_windows(windows, default);
            }
        }
    }
    model
}

fn find_cursor_effort_json(options: &[Value]) -> Option<&Value> {
    let candidates: Vec<&Value> = options
        .iter()
        .filter(|option| is_cursor_effort_json(option))
        .collect();
    candidates
        .iter()
        .copied()
        .find(|option| json_option_category(option).eq_ignore_ascii_case("model_option"))
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .find(|option| json_option_id(option).eq_ignore_ascii_case("effort"))
        })
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .find(|option| json_option_category(option).eq_ignore_ascii_case("thought_level"))
        })
        .or_else(|| candidates.first().copied())
}

fn is_cursor_effort_json(option: &Value) -> bool {
    if !json_option_type(option).eq_ignore_ascii_case("select") {
        return false;
    }
    let id = json_option_id(option).to_ascii_lowercase();
    let name = json_option_name(option).to_ascii_lowercase();
    id == "effort"
        || id == "reasoning"
        || name == "effort"
        || name == "reasoning"
        || name.contains("effort")
        || name.contains("reasoning")
}

fn is_cursor_fast_json(option: &Value) -> bool {
    if !json_option_category(option).eq_ignore_ascii_case("model_config") {
        return false;
    }
    let id = json_option_id(option).to_ascii_lowercase();
    let name = json_option_name(option).to_ascii_lowercase();
    id == "fast" || name == "fast" || name.contains("fast mode")
}

fn is_cursor_context_json(option: &Value) -> bool {
    if !json_option_category(option).eq_ignore_ascii_case("model_config") {
        return false;
    }
    let id = json_option_id(option).to_ascii_lowercase();
    let name = json_option_name(option).to_ascii_lowercase();
    id == "context" || id == "context_size" || name.contains("context")
}

fn json_option_id(option: &Value) -> &str {
    option
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn json_option_name(option: &Value) -> &str {
    option
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn json_option_category(option: &Value) -> &str {
    option
        .get("category")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn json_option_type(option: &Value) -> &str {
    option
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn json_current_string(option: &Value) -> Option<String> {
    match option.get("currentValue") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn json_current_bool(option: &Value) -> Option<bool> {
    match option.get("currentValue") {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("true") => Some(true),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn json_select_choices(option: &Value) -> Vec<(String, String)> {
    let Some(entries) = option.get("options").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut choices = Vec::new();
    for entry in entries {
        if let Some(value) = entry
            .get("value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or(value);
            choices.push((value.to_owned(), name.to_owned()));
            continue;
        }
        let Some(nested) = entry.get("options").and_then(Value::as_array) else {
            continue;
        };
        for nested_entry in nested {
            let Some(value) = nested_entry
                .get("value")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let name = nested_entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(value);
            choices.push((value.to_owned(), name.to_owned()));
        }
    }
    choices
}

fn cursor_select_label(id: &str, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name.contains(":icon-") {
        if id.is_empty() {
            name.to_owned()
        } else {
            reasoning_effort_label(id)
        }
    } else {
        name.to_owned()
    }
}

fn parse_cursor_models(output: &str) -> Vec<ProviderModel> {
    strip_ansi(output)
        .lines()
        .filter_map(|line| {
            let mut line = line.trim();
            if line.is_empty()
                || line.ends_with(':')
                || matches!(
                    line.to_ascii_lowercase().as_str(),
                    "model" | "models" | "available models"
                )
            {
                return None;
            }
            line = line
                .strip_prefix('*')
                .or_else(|| line.strip_prefix('-'))
                .or_else(|| line.strip_prefix('•'))
                .map(str::trim)
                .unwrap_or(line);
            let is_default = line.to_ascii_lowercase().contains("(default)");
            let line = line
                .replace("(default)", "")
                .replace("(Default)", "")
                .replace("(current)", "")
                .replace("(Current)", "");
            let line = line.trim();
            if line.is_empty() || line.contains("Usage:") || line.contains("error:") {
                return None;
            }
            let (id, label) = line
                .split_once('\t')
                .or_else(|| line.split_once("  "))
                .or_else(|| line.split_once(" - "))
                .map(|(id, label)| (id.trim(), label.trim()))
                .filter(|(id, _)| !id.is_empty())
                .unwrap_or((line, line));
            if id.split_whitespace().count() != 1 {
                return None;
            }
            let name = if label == id {
                display_name_from_slug(id)
            } else {
                label.to_owned()
            };
            let model = ProviderModel::new(id, name);
            Some(if is_default { model.default() } else { model })
        })
        .collect()
}

fn discover_opencode_models(binary: &Path) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    // `--verbose` prints each model's full metadata as JSON, which is the only
    // place the CLI exposes `variants` — the reasoning-effort ladder. The bare
    // listing is ids alone, so parsing it left every OpenCode model without an
    // effort control.
    let command = command.args(["models", "--verbose"]);
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let verbose = parse_opencode_verbose_models(&stdout);
    if verbose.is_empty() {
        // An older CLI without `--verbose` still prints the plain listing.
        return parse_opencode_models(&stdout);
    }
    verbose
}

/// Reads the JSON blocks `opencode models --verbose` prints, one per model.
///
/// Each block is preceded by a `provider/model` heading, but the block itself
/// carries `providerID` and `id`, so the headings are ignored and only
/// balanced top-level objects are decoded. A block that fails to parse is
/// skipped rather than failing the whole catalogue.
fn parse_opencode_verbose_models(output: &str) -> Vec<ProviderModel> {
    let cleaned = strip_ansi(output);
    let mut models = Vec::new();
    let mut block = String::new();
    let mut depth = 0_usize;
    for line in cleaned.lines() {
        let trimmed = line.trim_end();
        if depth == 0 && trimmed.trim() != "{" {
            continue;
        }
        depth += trimmed.matches('{').count();
        depth = depth.saturating_sub(trimmed.matches('}').count());
        block.push_str(trimmed);
        block.push('\n');
        if depth == 0 {
            if let Some(model) = parse_opencode_verbose_model(&block) {
                models.push(model);
            }
            block.clear();
        }
    }
    models
}

fn parse_opencode_verbose_model(block: &str) -> Option<ProviderModel> {
    let value: Value = serde_json::from_str(block).ok()?;
    let provider = value.get("providerID").and_then(Value::as_str)?.trim();
    let id = value.get("id").and_then(Value::as_str)?.trim();
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| display_name_from_slug(id));
    let model = ProviderModel::new(format!("{provider}/{id}"), name)
        .sub_provider(display_name_from_slug(provider));
    let variants = value
        .get("variants")
        .and_then(Value::as_object)
        .map(|variants| variants.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    Some(with_variant_efforts(model, variants))
}

fn discover_deepseek_catalog(
    binary: &Path,
) -> (Vec<ProviderModel>, Option<Vec<ProviderAgentPreset>>) {
    let Ok(server) = crate::deepseek_pool::acquire(binary) else {
        return (Vec::new(), None);
    };
    let models = server
        .rpc("llm.models", json!({}))
        .map(|catalog| parse_deepseek_model_catalog(&catalog))
        .unwrap_or_default();
    let presets = server
        .rpc("agentPreset.list", json!({}))
        .ok()
        .map(|roster| parse_deepseek_agent_presets(&roster));
    (models, presets)
}

fn parse_deepseek_agent_presets(roster: &Value) -> Vec<ProviderAgentPreset> {
    roster
        .get("presets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        // Harness deliberately leaves broken presets in its management
        // roster, but its own session picker excludes them.
        .filter(|value| value.get("broken").is_none())
        .filter_map(|value| {
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())?;
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(id);
            let mut preset = ProviderAgentPreset::new(id, name);
            preset.is_default = value
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            preset.is_custom = value.get("trust").and_then(Value::as_str) == Some("user");
            preset.description = value
                .get("description")
                .and_then(Value::as_str)
                .filter(|description| !description.trim().is_empty())
                .map(str::to_owned);
            Some(preset)
        })
        .collect()
}

fn parse_deepseek_model_catalog(catalog: &Value) -> Vec<ProviderModel> {
    catalog
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            let provider_id = group
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty());
            let provider_name = group
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty());
            let models = group.get("models").and_then(Value::as_array);
            provider_id
                .zip(provider_name)
                .zip(models)
                .into_iter()
                .flat_map(|((provider_id, provider_name), models)| {
                    models.iter().filter_map(move |value| {
                        let model_id = value
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.trim().is_empty())?;
                        let name = value
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| !name.trim().is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| display_name_from_slug(model_id));
                        let mut model =
                            ProviderModel::new(format!("{provider_id}/{model_id}"), name)
                                .sub_provider(provider_name);
                        if let Some(reasoning) = value.get("reasoning") {
                            model.reasoning_efforts = reasoning
                                .get("efforts")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|effort| {
                                    let id = effort.get("id").and_then(Value::as_str)?;
                                    let name = effort
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .filter(|name| !name.trim().is_empty())
                                        .unwrap_or(id);
                                    Some(
                                        ProviderModelOption::new(id, name).description(
                                            effort
                                                .get("description")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default(),
                                        ),
                                    )
                                })
                                .collect();
                            model.default_reasoning_effort = reasoning
                                .get("defaultEffort")
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                        }
                        Some(model)
                    })
                })
        })
        .collect()
}

/// Droid serves its catalog only inside a session, and its per-model effort
/// ladders live in the session's config options, which change with the
/// selected model. Discovery therefore opens one local-only session (a
/// client-generated `_meta.sessionId` makes Droid skip the Factory-side
/// record), then for every advertised model sets it and re-reads the session's
/// `reasoning_effort` option through `session/resume`. The agent is the only
/// source of these ladders — new releases and BYOK routes are covered the day
/// they appear — and the whole enriched catalog is cached to keep the loop
/// rare. Any failed step aborts so a degraded catalog is never cached.
fn discover_droid_models(binary: &Path) -> Vec<ProviderModel> {
    let Ok(cwd) = crate::acp_session::catalog_working_directory() else {
        return Vec::new();
    };
    let Ok(agent) = crate::driver::catalog_agent(ProviderKind::Droid, binary, &cwd) else {
        return Vec::new();
    };
    let request = Client
        .builder()
        .name("waku-droid-model-discovery")
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(ClientCapabilities::new().terminal(false))
                        .client_info(Implementation::new("waku", env!("CARGO_PKG_VERSION"))),
                )
                .block_task()
                .await?;
            let session = connection
                .send_request(UntypedMessage::new(
                    "session/new",
                    json!({
                        "cwd": cwd.to_string_lossy(),
                        "mcpServers": [],
                        "_meta": { "sessionId": uuid::Uuid::new_v4().to_string() },
                    }),
                )?)
                .block_task()
                .await?;
            let session_id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Droid answered session/new without a session id"))?
                .to_owned();
            let default_model = session
                .pointer("/models/currentModelId")
                .and_then(Value::as_str)
                .unwrap_or("auto");
            let entries = session
                .pointer("/models/availableModels")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut catalog = Vec::with_capacity(entries.len());
            for entry in &entries {
                let Some(id) = entry
                    .get("modelId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                connection
                    .send_request(UntypedMessage::new(
                        "session/set_config_option",
                        json!({
                            "sessionId": session_id,
                            "configId": "model",
                            "value": id,
                        }),
                    )?)
                    .block_task()
                    .await?;
                let refreshed = connection
                    .send_request(UntypedMessage::new(
                        "session/resume",
                        json!({ "sessionId": session_id }),
                    )?)
                    .block_task()
                    .await?;
                catalog.push(json!({
                    "modelId": id,
                    "name": entry.get("name").and_then(Value::as_str).unwrap_or(id),
                    "isDefault": id == default_model,
                    "efforts": droid_effort_state(&refreshed),
                }));
            }
            Ok(Value::Array(catalog))
        });
    let response = smol::block_on(smol::future::race(
        async move { request.await.map_err(|_| ()) },
        async move {
            smol::Timer::after(DROID_ACP_DISCOVERY_TIMEOUT).await;
            Err(())
        },
    ));
    response
        .ok()
        .and_then(|value| value.as_array().cloned())
        .map(|entries| parse_droid_models(&entries))
        .unwrap_or_default()
}

/// The `reasoning_effort` session option of a refreshed `session/resume`
/// answer: its provider-named choices and the value the selected model starts
/// on. `Value::Null` when the session carries no such option, which leaves the
/// model without an effort menu.
fn droid_effort_state(session: &Value) -> Value {
    let effort = session
        .get("configOptions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| {
            option.get("id").and_then(Value::as_str) == Some("reasoning_effort")
                || option.get("category").and_then(Value::as_str) == Some("thought_level")
        });
    let Some(effort) = effort else {
        return Value::Null;
    };
    let options = effort
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            let id = choice.get("value").and_then(Value::as_str)?;
            let label = choice
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            Some(json!({ "id": id, "label": label }))
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return Value::Null;
    }
    let current = effort
        .get("currentValue")
        .and_then(Value::as_str)
        .filter(|current| options.iter().any(|option| option["id"] == *current))
        .map(str::to_owned)
        .or_else(|| {
            options
                .first()
                .and_then(|option| option["id"].as_str())
                .map(str::to_owned)
        });
    json!({ "options": options, "default": current })
}

fn parse_droid_models(entries: &[Value]) -> Vec<ProviderModel> {
    entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .get("modelId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            let mut model = ProviderModel::new(id, name);
            if entry.get("isDefault").and_then(Value::as_bool) == Some(true) {
                model = model.default();
            }
            let efforts = entry.get("efforts").filter(|efforts| !efforts.is_null());
            let options = efforts
                .and_then(|efforts| efforts.get("options"))
                .and_then(Value::as_array);
            let default = efforts
                .and_then(|efforts| efforts.get("default"))
                .and_then(Value::as_str);
            if let Some(options) = options
                // A menu of one is not a choice: Droid answers a lone "None"
                // for models with no effort control, such as the Factory-router
                // `auto`, and offering it would only let users pick the value
                // the model already has.
                .filter(|options| options.len() >= 2)
                .filter(|options| {
                    default.is_some_and(|default| {
                        options.iter().any(|option| option["id"] == *default)
                    })
                })
            {
                model = model.reasoning(
                    options.iter().map(|option| {
                        ProviderModelOption::new(
                            option["id"].as_str().unwrap_or_default(),
                            option["label"].as_str().unwrap_or_default(),
                        )
                    }),
                    default.unwrap_or_default(),
                );
            }
            Some(model)
        })
        .collect()
}

fn discover_fx_models(binary: &Path) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    let command = command.args(["models", "--json"]);
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    let Ok(catalog) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Vec::new();
    };
    parse_fx_models(&catalog, discover_fx_default_model(binary).as_deref())
}

fn discover_fx_default_model(binary: &Path) -> Option<String> {
    let mut command = crate::command_env::command(binary);
    let command = command.args(["status", "--json"]);
    let output = crate::command_env::output(command).ok()?;
    let status = serde_json::from_slice::<Value>(&output.stdout).ok()?;
    status
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

fn parse_fx_models(catalog: &Value, default_model: Option<&str>) -> Vec<ProviderModel> {
    catalog
        .get("ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| {
            let (provider, model_id) = id
                .split_once('/')
                .map_or((None, id), |(provider, model)| (Some(provider), model));
            let mut model = ProviderModel::new(id, display_name_from_slug(model_id));
            if let Some(provider) = provider.filter(|provider| !provider.is_empty()) {
                model = model.sub_provider(display_name_from_slug(provider));
            }
            model.is_default = default_model == Some(id);
            model
        })
        .collect()
}

fn parse_opencode_models(output: &str) -> Vec<ProviderModel> {
    output
        .lines()
        .filter_map(|line| {
            let id = strip_ansi(line).trim().to_owned();
            if id.is_empty() || id.split_whitespace().count() != 1 || !id.contains('/') {
                return None;
            }
            let (provider, model) = id.split_once('/')?;
            if provider.is_empty() || model.is_empty() {
                return None;
            }
            Some(
                ProviderModel::new(id.clone(), display_name_from_slug(model))
                    .sub_provider(display_name_from_slug(provider)),
            )
        })
        .collect()
}

fn discover_grok_models(binary: &Path) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    let command = command.arg("models");
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_grok_models(&combined)
}

fn parse_grok_models(output: &str) -> Vec<ProviderModel> {
    let cleaned = strip_ansi(output);
    let default_model = cleaned.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Default model:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    });
    let mut in_models = false;
    cleaned
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line == "Available models:" {
                in_models = true;
                return None;
            }
            if !in_models {
                return None;
            }
            // The default is starred and the rest are dashed:
            //   * grok-4.6 (default)
            //   - grok-4.5
            let entry = line
                .strip_prefix('*')
                .or_else(|| line.strip_prefix('-'))?
                .trim();
            let id = entry.strip_suffix("(default)").unwrap_or(entry).trim();
            if id.is_empty() || id.chars().any(char::is_whitespace) {
                return None;
            }
            let mut model = ProviderModel::new(id, display_name_from_slug(id));
            model.is_default = default_model.as_deref() == Some(id);
            Some(grok_reasoning_model(model))
        })
        .collect()
}

/// Kimi Code resolves models through its own provider config, which covers the
/// managed Kimi plan as well as any registry the user imported. `--json` is the
/// catalog itself; it omits the configured default, so the human-readable
/// listing supplies that single field.
fn discover_kimi_models(binary: &Path) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    let command = command.args(["provider", "list", "--json"]);
    let Ok(output) = crate::command_env::output(command) else {
        return Vec::new();
    };
    let Ok(catalog) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Vec::new();
    };
    parse_kimi_models(&catalog, discover_kimi_default_model(binary).as_deref())
}

fn discover_kimi_default_model(binary: &Path) -> Option<String> {
    let mut command = crate::command_env::command(binary);
    let command = command.args(["provider", "list"]);
    let output = crate::command_env::output(command).ok()?;
    parse_kimi_default_model(&String::from_utf8_lossy(&output.stdout))
}

fn parse_kimi_default_model(output: &str) -> Option<String> {
    strip_ansi(output).lines().find_map(|line| {
        line.trim()
            .strip_prefix("Default model:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_kimi_models(catalog: &Value, default_model: Option<&str>) -> Vec<ProviderModel> {
    catalog
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(id, _)| !id.trim().is_empty())
        .map(|(id, value)| {
            // Only the managed Kimi models carry a display name. An imported
            // registry names its models by bare id, and the alias prefix is
            // what tells two providers' catalogs apart in the picker.
            let (alias_provider, alias_model) = id
                .split_once('/')
                .map_or((None, id.as_str()), |(prefix, rest)| (Some(prefix), rest));
            let name = value
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    display_name_from_slug(
                        value
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|model| !model.is_empty())
                            .unwrap_or(alias_model),
                    )
                });
            let mut model = ProviderModel::new(id, name);
            if let Some(prefix) = alias_provider.filter(|prefix| !prefix.trim().is_empty()) {
                model = model.sub_provider(display_name_from_slug(prefix));
            }
            model.is_default = default_model == Some(id.as_str());
            // Only the K3 family exposes thinking levels; the rest report a
            // single always-on state that is not a user choice.
            model.reasoning_efforts = value
                .get("supportEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|effort| !effort.is_empty())
                .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort)))
                .collect();
            if !model.reasoning_efforts.is_empty() {
                model.default_reasoning_effort = value
                    .get("defaultEffort")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|effort| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == *effort)
                    })
                    .map(str::to_owned);
            }
            model
        })
        .collect()
}

/// Pi and Oh My Pi share the RPC catalog commands but not the flags that keep
/// a probe cheap, nor the shape of a model's thinking metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
enum PiDialect {
    Pi,
    OhMyPi,
}

impl PiDialect {
    fn probe_args(self) -> &'static [&'static str] {
        match self {
            Self::Pi => &[
                "--mode",
                "rpc",
                "--no-session",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
            ],
            // Oh My Pi rejects unknown flags outright, and spells context
            // files `--no-rules`.
            Self::OhMyPi => &[
                "--mode",
                "rpc",
                "--no-session",
                "--no-skills",
                "--no-rules",
                "--no-extensions",
            ],
        }
    }
}

fn discover_pi_models(binary: &Path, dialect: PiDialect) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    if dialect == PiDialect::Pi {
        // Oh My Pi has no such opt-out; it gates its update check on a setting.
        command.env("PI_SKIP_VERSION_CHECK", "1");
    }
    let command = command
        .args(dialect.probe_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = crate::command_env::spawn(command) else {
        return Vec::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

    let models_request = json!({"id": "waku-models", "type": "get_available_models"});
    let result = if write_json_line(&mut stdin, &models_request).is_ok()
        && let Some(models) = recv_pi_rpc_response(&rx, "waku-models", PI_RPC_TIMEOUT)
    {
        // The catalog is useful even when an older Pi cannot report its
        // current state. State only marks the default model and thinking
        // level, so ask for it after the required model response.
        let state_request = json!({"id": "waku-state", "type": "get_state"});
        let state = if write_json_line(&mut stdin, &state_request).is_ok() {
            recv_pi_rpc_response(&rx, "waku-state", PI_RPC_TIMEOUT).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        parse_pi_model_response(dialect, &state, &models)
    } else {
        Vec::new()
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn parse_pi_model_response(
    dialect: PiDialect,
    state: &Value,
    response: &Value,
) -> Vec<ProviderModel> {
    let default_provider = state
        .pointer("/data/model/provider")
        .and_then(Value::as_str);
    let default_model = state.pointer("/data/model/id").and_then(Value::as_str);
    let default_slug = default_provider
        .zip(default_model)
        .map(|(provider, model)| format!("{provider}/{model}"));
    let current_thinking = state.pointer("/data/thinkingLevel").and_then(Value::as_str);

    response
        .pointer("/data/models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let provider = value.get("provider").and_then(Value::as_str)?;
            let model_id = value.get("id").and_then(Value::as_str)?;
            if provider.trim().is_empty() || model_id.trim().is_empty() {
                return None;
            }
            let slug = format!("{provider}/{model_id}");
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| display_name_from_slug(model_id));
            let mut model = ProviderModel::new(slug.clone(), name)
                .sub_provider(display_name_from_slug(provider));
            model.is_default = default_slug.as_deref() == Some(slug.as_str());
            model.reasoning_efforts = pi_reasoning_options(dialect, value);
            if !model.reasoning_efforts.is_empty() {
                let preferred = model
                    .is_default
                    .then_some(current_thinking)
                    .flatten()
                    .filter(|effort| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == **effort)
                    })
                    .or_else(|| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == "medium")
                            .then_some("medium")
                    })
                    .or_else(|| {
                        model
                            .reasoning_efforts
                            .first()
                            .map(|option| option.id.as_str())
                    });
                model.default_reasoning_effort = preferred.map(str::to_owned);
            }
            Some(model)
        })
        .collect()
}

const PI_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn pi_reasoning_options(dialect: PiDialect, model: &Value) -> Vec<ProviderModelOption> {
    if model.get("reasoning").and_then(Value::as_bool) != Some(true) {
        return Vec::new();
    }
    if dialect == PiDialect::OhMyPi {
        // Oh My Pi advertises the levels a model actually honors. `off` is
        // never in that list because it bypasses provider mapping entirely,
        // but it is always accepted.
        let Some(efforts) = model.pointer("/thinking/efforts").and_then(Value::as_array) else {
            return Vec::new();
        };
        return PI_THINKING_LEVELS
            .into_iter()
            .filter(|level| {
                *level == "off"
                    || efforts
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|effort| effort == *level)
            })
            .map(|level| ProviderModelOption::new(level, reasoning_effort_label(level)))
            .collect();
    }
    let level_map = model.get("thinkingLevelMap").and_then(Value::as_object);
    PI_THINKING_LEVELS
        .into_iter()
        .filter(|level| {
            let mapped = level_map.and_then(|map| map.get(*level));
            if matches!(*level, "xhigh" | "max") {
                mapped.is_some_and(|value| !value.is_null())
            } else {
                mapped.is_none_or(|value| !value.is_null())
            }
        })
        .map(|level| ProviderModelOption::new(level, reasoning_effort_label(level)))
        .collect()
}

fn discover_codex_models(binary: &Path) -> Vec<ProviderModel> {
    let mut command = crate::command_env::command(binary);
    let command = command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = crate::command_env::spawn(command) else {
        return Vec::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

    let initialize = json!({
        "method": "initialize",
        "id": 0,
        "params": {
            "clientInfo": {
                "name": "waku",
                "title": "Waku",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "experimentalApi": true
            }
        }
    });
    if write_json_line(&mut stdin, &initialize).is_err()
        || recv_rpc_response(&rx, 0, CODEX_RPC_TIMEOUT).is_none()
        || write_json_line(&mut stdin, &json!({"method": "initialized", "params": {}})).is_err()
    {
        let _ = child.kill();
        return Vec::new();
    }

    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for request_id in 1..=32_u64 {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        if write_json_line(
            &mut stdin,
            &json!({"method": "model/list", "id": request_id, "params": params}),
        )
        .is_err()
        {
            break;
        }
        let Some(response) = recv_rpc_response(&rx, request_id, CODEX_RPC_TIMEOUT) else {
            break;
        };
        models.extend(parse_codex_model_response(&response));
        cursor = response
            .pointer("/result/nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    models
}

fn parse_codex_model_response(response: &Value) -> Vec<ProviderModel> {
    response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let id = value.get("model").and_then(Value::as_str)?;
            let name = value
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| display_name_from_slug(id));
            let mut model = ProviderModel::new(id, normalize_codex_name(&name));
            model.is_default = value
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            model.reasoning_efforts = value
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let id = option.get("reasoningEffort").and_then(Value::as_str)?;
                    let description = option
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(
                        ProviderModelOption::new(id, reasoning_effort_label(id))
                            .description(description),
                    )
                })
                .collect();
            model.default_reasoning_effort = value
                .get("defaultReasoningEffort")
                .and_then(Value::as_str)
                .map(str::to_owned);
            model.service_tiers = value
                .get("serviceTiers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tier| {
                    let id = tier.get("id").and_then(Value::as_str)?;
                    let name = tier
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or(id);
                    let description = tier
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(ProviderModelOption::new(id, name).description(description))
                })
                .collect();
            if model.service_tiers.is_empty() {
                model.service_tiers = value
                    .get("additionalSpeedTiers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|id| {
                        ProviderModelOption::new(
                            id,
                            if id == "fast" {
                                "Fast".to_owned()
                            } else {
                                display_name_from_slug(id)
                            },
                        )
                    })
                    .collect();
            }
            model.default_service_tier = value
                .get("defaultServiceTier")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| (!model.service_tiers.is_empty()).then(|| "default".to_owned()));
            Some(model)
        })
        .collect()
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
        other => display_name_from_slug(other),
    }
}

/// Attaches a model's OpenCode "variants" as its reasoning-effort ladder.
///
/// Both OpenCode majors express reasoning effort as a per-model variant whose
/// id is drawn from the same vocabulary Waku already labels
/// (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max`, plus provider-specific
/// ones such as `thinking`), and the chosen id is sent verbatim — as the v1
/// message body's `variant` and as v2's `ModelRef::variant`. Models with no
/// variants keep an empty ladder, which is what hides the control.
///
/// The default is the strongest offered rather than the weakest: a variant
/// list is opt-in per model, so a model that publishes one is a reasoning
/// model and the ladder's top is the reason to pick it.
pub(crate) fn with_variant_efforts<'a>(
    model: ProviderModel,
    variants: impl IntoIterator<Item = &'a str>,
) -> ProviderModel {
    const STRENGTH: [&str; 8] = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    let ids: Vec<String> = variants
        .into_iter()
        .map(str::trim)
        .filter(|variant| !variant.is_empty())
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        return model;
    }
    let rank = |id: &str| STRENGTH.iter().position(|known| *known == id);
    // Preserve the provider's own ordering; only the default is ranked, and a
    // ladder of entirely unknown ids falls back to the last one offered.
    let default = ids
        .iter()
        .filter(|id| rank(id).is_some())
        .max_by_key(|id| rank(id))
        .or_else(|| ids.last())
        .cloned();
    let options = ids
        .iter()
        .map(|id| ProviderModelOption::new(id.clone(), reasoning_effort_label(id)));
    match default {
        Some(default) => model.reasoning(options, default),
        None => model,
    }
}

fn reasoning_options<const N: usize>(efforts: [&str; N]) -> Vec<ProviderModelOption> {
    efforts
        .into_iter()
        .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort)))
        .collect()
}

/// The hardcoded reasoning menu is limited to the exact built-in models it
/// was verified against. `grok models` also lists user-defined custom models,
/// whose effort support is not knowable from the ID, so they get no menu.
fn grok_reasoning_model(model: ProviderModel) -> ProviderModel {
    match waku_protocol::model_catalog::grok_model_reasoning_efforts(&model.id) {
        Some(efforts) => model.reasoning(
            efforts
                .iter()
                .copied()
                .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort))),
            "high",
        ),
        None => model,
    }
}

fn claude_reasoning_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel::new(id, name).reasoning(
        reasoning_options(["low", "medium", "high", "xhigh", "max"]),
        "high",
    )
}

/// `ultracode` is an effort value Claude Code accepts alongside the ordinary
/// ladder: it resolves to xhigh and turns on standing dynamic-workflow
/// orchestration for that session. It therefore needs an xhigh-capable model —
/// older ones clamp xhigh back to high, which would leave the entry inert.
fn claude_ultracode_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel::new(id, name).reasoning(
        reasoning_options(["low", "medium", "high", "xhigh", "max", "ultracode"]),
        "high",
    )
}

/// Claude Code serves a 200K window by default and reaches the 1M one through
/// a `[1m]` model-id suffix, so the window is a per-session trait rather than a
/// separate model. The CLI refuses the suffix on Claude 3, Opus 4.0/4.1/4.5,
/// and Haiku 4.5, so only the models that honor it carry the choice.
fn claude_long_context(model: ProviderModel) -> ProviderModel {
    model.context_windows(
        [
            ProviderModelOption::new("200k", tr!("model_option.context_200k")),
            ProviderModelOption::new("1m", tr!("model_option.context_1m")),
        ],
        "200k",
    )
}

fn recv_rpc_response(rx: &Receiver<Value>, id: u64, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let value = rx.recv_timeout(remaining).ok()?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Some(value);
        }
    }
}

fn recv_pi_rpc_response(rx: &Receiver<Value>, id: &str, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let value = rx.recv_timeout(remaining).ok()?;
        if value.get("id").and_then(Value::as_str) == Some(id) {
            return (value.get("success").and_then(Value::as_bool) == Some(true)).then_some(value);
        }
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn normalize_codex_name(name: &str) -> String {
    let name = if name
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gpt"))
    {
        format!("GPT{}", &name[3..])
    } else {
        name.to_owned()
    };
    let mut capitalize_next = false;
    name.chars()
        .flat_map(|char| {
            if capitalize_next {
                capitalize_next = false;
                char.to_uppercase().collect::<Vec<_>>()
            } else {
                capitalize_next = char == '-';
                vec![char]
            }
        })
        .collect()
}

pub(crate) fn display_name_from_slug(slug: &str) -> String {
    let words = slug
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "gpt" => "GPT".to_owned(),
            "ai" => "AI".to_owned(),
            "xai" => "xAI".to_owned(),
            _ if part
                .chars()
                .all(|char| char.is_ascii_digit() || char == '.') =>
            {
                part.to_owned()
            }
            _ => {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        })
        .collect::<Vec<_>>();
    if words.first().is_some_and(|word| word == "GPT") {
        words.join("-")
    } else {
        words.join(" ")
    }
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(char);
        }
    }
    output
}

fn deduplicate(models: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_fake_model_cli(name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!(
            "waku-{name}-{}-model-discovery.sh",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn model_cache_round_trips_and_rejects_empty_or_invalid_files() {
        let directory =
            std::env::temp_dir().join(format!("waku-model-cache-test-{}", std::process::id()));
        let path = directory.join("codex.json");
        let models = vec![
            ProviderModel::new("gpt-5.6-sol", "GPT-5.6-Sol")
                .default()
                .reasoning(reasoning_options(["low", "high"]), "high"),
        ];

        assert_eq!(read_models_file(&path), None);
        write_models_file(&path, &models).unwrap();
        assert_eq!(read_models_file(&path), Some(models));

        write_models_file(&path, &[]).unwrap();
        assert_eq!(read_models_file(&path), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_models_file(&path), None);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn amp_catalog_uses_agent_modes_and_medium_by_default() {
        let models = fallback_models(ProviderKind::Amp);

        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "ultra"]
        );
        assert!(models[1].is_default);
        assert_eq!(models[1].service_tiers[0].id, "fast");
    }

    #[test]
    fn claude_catalog_offers_ultracode_only_on_xhigh_capable_models() {
        let models = fallback_models(ProviderKind::Claude);
        let efforts = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .map(|model| {
                    model
                        .reasoning_efforts
                        .iter()
                        .map(|option| option.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        assert_eq!(
            efforts("claude-opus-5"),
            ["low", "medium", "high", "xhigh", "max", "ultracode"]
        );
        assert_eq!(
            efforts("claude-sonnet-4-6"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(efforts("claude-haiku-4-5").is_empty());
    }

    #[test]
    fn parses_claude_initialization_models_without_repeating_resolved_ids() {
        let models = parse_claude_models(&json!({
            "type": "control_response",
            "response": {"response": {"models": [
                {
                    "value": "default",
                    "resolvedModel": "deepseek-v4-pro",
                    "displayName": "DeepSeek V4 Pro",
                    "supportsEffort": true,
                    "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]
                },
                {
                    "value": "sonnet",
                    "resolvedModel": "claude-sonnet-5",
                    "displayName": "Sonnet",
                    "supportsEffort": false
                },
                {"value": "  ", "displayName": "ignored"}
            ]}}
        }));

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "default");
        assert_eq!(models[0].name, "DeepSeek V4 Pro");
        assert!(models[0].is_default);
        assert_eq!(
            models[0]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh", "max", "ultracode"]
        );
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(models[1].name, "Sonnet");
        assert!(models[1].reasoning_efforts.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn discovers_claude_models_from_sessionless_initialization() {
        let binary = write_fake_model_cli(
            "claude",
            r#"#!/bin/sh
read -r request
printf '%s\n' '{"type":"control_response","response":{"request_id":"waku-initialize-catalog","response":{"models":[{"value":"cc-switch-model","resolvedModel":"cc-switch-model","displayName":"CC Switch Model"}]}}}'
"#,
        );

        let models = discover_claude_models(&binary);

        let _ = std::fs::remove_file(binary);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "cc-switch-model");
        assert_eq!(models[0].name, "CC Switch Model");
    }

    #[test]
    fn cursor_catalog_falls_back_to_provider_owned_auto() {
        let models = fallback_models(ProviderKind::Cursor);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "auto");
        assert!(models[0].is_default);
    }

    /// The exact shape `opencode models --verbose` prints, trimmed to the
    /// fields Waku reads. `variants` is the reasoning-effort ladder and is the
    /// only reason to parse the verbose form at all.
    #[test]
    fn opencode_verbose_models_expose_their_variants_as_reasoning_efforts() {
        let output = r#"opencode-go/deepseek-v4-flash
{
  "id": "deepseek-v4-flash",
  "providerID": "opencode-go",
  "name": "DeepSeek V4 Flash",
  "variants": {
    "low": { "reasoningEffort": "low" },
    "high": { "reasoningEffort": "high" },
    "max": { "reasoningEffort": "max" }
  }
}
opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "variants": {}
}
"#;
        let models = parse_opencode_verbose_models(output);
        assert_eq!(models.len(), 2, "{models:?}");

        let flash = &models[0];
        assert_eq!(flash.id, "opencode-go/deepseek-v4-flash");
        assert_eq!(flash.name, "DeepSeek V4 Flash");
        assert_eq!(
            flash
                .reasoning_efforts
                .iter()
                .map(|effort| effort.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "high", "max"],
            "provider ordering is preserved"
        );
        assert_eq!(flash.default_reasoning_effort.as_deref(), Some("max"));

        // A model with no ladder keeps none, which is what hides the control.
        assert!(models[1].reasoning_efforts.is_empty());
        assert!(models[1].default_reasoning_effort.is_none());
    }

    /// A CLI too old for `--verbose` still prints the plain listing, so the
    /// parser must not turn that into an empty catalogue.
    #[test]
    fn opencode_verbose_parsing_ignores_a_plain_listing() {
        assert!(
            parse_opencode_verbose_models(
                "opencode-go/deepseek-v4-flash
"
            )
            .is_empty()
        );
        assert_eq!(
            parse_opencode_models(
                "opencode-go/deepseek-v4-flash
"
            )
            .len(),
            1
        );
    }

    #[test]
    fn variant_efforts_default_to_the_strongest_offered() {
        let model = with_variant_efforts(ProviderModel::new("a/b", "B"), ["none", "thinking"]);
        assert_eq!(
            model
                .reasoning_efforts
                .iter()
                .map(|effort| effort.id.as_str())
                .collect::<Vec<_>>(),
            ["none", "thinking"]
        );
        // `thinking` is not on the known strength ladder, so the only ranked
        // id wins; an unranked id never becomes the default while one ranks.
        assert_eq!(model.default_reasoning_effort.as_deref(), Some("none"));
    }

    #[test]
    fn parses_opencode_provider_qualified_models() {
        let models = parse_opencode_models(
            "opencode/big-pickle\n\u{1b}[32mgithub-copilot/gpt-5.4\u{1b}[0m\nnoise here\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].id, "github-copilot/gpt-5.4");
        assert_eq!(models[1].name, "GPT-5.4");
        assert_eq!(models[1].sub_provider.as_deref(), Some("Github Copilot"));
    }

    #[test]
    fn parses_fx_json_catalog_and_current_default() {
        let models = parse_fx_models(
            &json!({
                "kind": "models",
                "count": 3,
                "ids": [
                    "anthropic/claude-sonnet-5",
                    "openai/gpt-5.6-sol",
                    "custom-model"
                ]
            }),
            Some("openai/gpt-5.6-sol"),
        );

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].name, "Claude Sonnet 5");
        assert_eq!(models[0].sub_provider.as_deref(), Some("Anthropic"));
        assert!(models[1].is_default);
        assert_eq!(models[2].name, "Custom Model");
        assert_eq!(models[2].sub_provider, None);
    }

    #[test]
    #[ignore = "requires an installed and configured Fx"]
    fn installed_fx_reports_models() {
        let binary = crate::command_env::find_executable("fx").expect("Fx is not installed");
        let models = discover_catalog(ProviderKind::Fx, &binary).0;
        assert!(!models.is_empty(), "the installed Fx reported no models");
        assert!(models.iter().any(|model| model.is_default));
    }

    /// Entry shapes captured from a live discovery: every model carries the
    /// effort state Droid answered for it. Models the agent gave no
    /// `reasoning_effort` option for, or answered with a lone choice (the
    /// Factory-router `auto` answers a single "None"), get no menu.
    #[test]
    fn parses_droid_catalog_with_per_model_effort_ladders() {
        let models = parse_droid_models(&[
            json!({
                "modelId": "auto",
                "name": "Auto Model",
                "isDefault": true,
                "efforts": {
                    "default": "none",
                    "options": [ { "id": "none", "label": "None" } ]
                },
            }),
            json!({
                "modelId": "claude-sonnet-5",
                "name": "Sonnet 5",
                "isDefault": false,
                "efforts": {
                    "default": "high",
                    "options": [
                        { "id": "off", "label": "Off" },
                        { "id": "low", "label": "Low" },
                        { "id": "medium", "label": "Medium" },
                        { "id": "high", "label": "High" },
                        { "id": "xhigh", "label": "Extra High" },
                        { "id": "max", "label": "Maximum" }
                    ]
                },
            }),
            json!({
                "modelId": "glm-5.3",
                "name": "GLM-5.3",
                "isDefault": false,
                "efforts": {
                    "default": "max",
                    "options": [
                        { "id": "low", "label": "Low" },
                        { "id": "high", "label": "High" },
                        { "id": "max", "label": "Maximum" }
                    ]
                },
            }),
            // A model whose effort answer lacks its default among the choices
            // is left menu-less rather than offered a dead rung.
            json!({
                "modelId": "byok-route",
                "name": "My Route",
                "isDefault": false,
                "efforts": {
                    "default": "medium",
                    "options": [ { "id": "high", "label": "High" } ]
                },
            }),
        ]);

        assert_eq!(models.len(), 4);
        assert!(models[0].is_default);
        assert!(models[0].reasoning_efforts.is_empty());
        let sonnet = &models[1];
        assert_eq!(
            sonnet
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["off", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(sonnet.default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            sonnet
                .reasoning_efforts
                .iter()
                .find(|option| option.id == "xhigh")
                .map(|option| option.label.as_str()),
            Some("Extra High")
        );
        assert_eq!(models[2].default_reasoning_effort.as_deref(), Some("max"));
        assert!(models[3].reasoning_efforts.is_empty());
    }

    #[test]
    fn parses_deepseek_host_model_groups_and_reasoning() {
        let models = parse_deepseek_model_catalog(&json!({
            "groups": [
                {
                    "id": "deepseek-official",
                    "name": "DeepSeek",
                    "models": [
                        {
                            "id": "deepseek-chat",
                            "name": "DeepSeek Chat",
                            "reasoning": {
                                "efforts": [
                                    {"id": "off", "name": "Off"},
                                    {"id": "high", "name": "High", "description": "Think longer"}
                                ],
                                "defaultEffort": "off"
                            }
                        }
                    ]
                },
                {"id": "empty", "name": "Empty", "models": []}
            ],
            "failures": []
        }));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "deepseek-official/deepseek-chat");
        assert_eq!(models[0].name, "DeepSeek Chat");
        assert_eq!(models[0].sub_provider.as_deref(), Some("DeepSeek"));
        assert_eq!(models[0].reasoning_efforts[1].id, "high");
        assert_eq!(
            models[0].reasoning_efforts[1].description.as_deref(),
            Some("Think longer")
        );
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("off"));
    }

    #[test]
    fn parses_only_selectable_deepseek_agent_presets() {
        let presets = parse_deepseek_agent_presets(&json!({
            "presets": [
                {
                    "id": "standard",
                    "trust": "system",
                    "isDefault": true,
                    "name": "Host-localized standard"
                },
                {
                    "id": "my-agent",
                    "trust": "user",
                    "isDefault": false,
                    "name": "My agent",
                    "description": "A local composition"
                },
                {
                    "id": "broken",
                    "trust": "user",
                    "isDefault": false,
                    "broken": "plugin failed"
                }
            ]
        }));

        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].id, "standard");
        assert!(presets[0].is_default);
        assert_eq!(presets[1].name, "My agent");
        assert!(presets[1].is_custom);
        assert_eq!(
            presets[1].description.as_deref(),
            Some("A local composition")
        );
    }

    #[test]
    #[ignore = "requires an installed DeepSeek Harness"]
    fn installed_deepseek_harness_reports_models() {
        let binary =
            crate::command_env::find_executable("dsh").expect("DeepSeek Harness is not installed");
        let models = discover_catalog(ProviderKind::DeepSeek, &binary).0;
        assert!(
            !models.is_empty(),
            "the installed DeepSeek Harness reported no models"
        );
    }

    #[test]
    fn parses_cursor_cli_models_and_default_marker() {
        let models = parse_cursor_models(
            "Available models:\n  * auto (default)\n  composer-2.5  Composer 2.5\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "auto");
        assert!(models[0].is_default);
        assert_eq!(models[1].id, "composer-2.5");
        assert_eq!(models[1].name, "Composer 2.5");
    }

    #[test]
    fn parses_cursor_cli_hyphen_separated_models() {
        // The format printed by cursor-agent since at least 2026.08.04:
        // `id - Label`, with `(default)` and `(current)` suffixes.
        let models = parse_cursor_models(
            "Available models\n\nauto - Auto (default)\ngpt-5.3-codex-low - Codex 5.3 Low\ncomposer-2.5 - Composer 2.5 (current)\nclaude-opus-5-thinking-high - Opus 5 1M Thinking\n",
        );
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [
                "auto",
                "gpt-5.3-codex-low",
                "composer-2.5",
                "claude-opus-5-thinking-high"
            ]
        );
        assert!(models[0].is_default);
        assert_eq!(models[0].name, "Auto");
        assert_eq!(models[1].name, "Codex 5.3 Low");
        assert_eq!(models[2].name, "Composer 2.5");
        assert!(!models[2].is_default);
    }

    #[test]
    fn parses_cursor_acp_available_models_with_effort_fast_and_context() {
        let models = parse_cursor_available_models(&json!({
            "models": [
                {
                    "value": "default",
                    "name": "Auto",
                    "configOptions": []
                },
                {
                    "value": "gpt-5.4",
                    "name": "GPT-5.4",
                    "configOptions": [
                        {
                            "id": "reasoning",
                            "name": "Reasoning",
                            "category": "thought_level",
                            "type": "select",
                            "currentValue": "medium",
                            "options": [
                                {"value": "none", "name": "None"},
                                {"value": "low", "name": "Low"},
                                {"value": "medium", "name": "Medium"},
                                {"value": "high", "name": "High"},
                                {"value": "extra-high", "name": "Extra High"}
                            ]
                        },
                        {
                            "id": "context",
                            "name": "Context",
                            "category": "model_config",
                            "type": "select",
                            "currentValue": "272k",
                            "options": [
                                {"value": "272k", "name": "272K"},
                                {"value": "1m", "name": "1M"}
                            ]
                        },
                        {
                            "id": "fast",
                            "name": "Fast",
                            "category": "model_config",
                            "type": "select",
                            "currentValue": "false",
                            "options": [
                                {"value": "false", "name": "Off"},
                                {"value": "true", "name": "Fast"}
                            ]
                        }
                    ]
                },
                {
                    "value": "claude-opus-4-6",
                    "name": "Opus 4.6",
                    "configOptions": [
                        {
                            "id": "reasoning",
                            "name": "Reasoning",
                            "category": "thought_level",
                            "type": "select",
                            "currentValue": "high",
                            "options": [
                                {"value": "low", "name": "Low"},
                                {"value": "medium", "name": "Medium"},
                                {"value": "high", "name": "High"}
                            ]
                        },
                        {
                            "id": "effort",
                            "name": "Effort",
                            "category": "model_option",
                            "type": "select",
                            "currentValue": "max",
                            "options": [
                                {"value": "low", "name": "Low"},
                                {"value": "medium", "name": "Medium"},
                                {"value": "high", "name": "High"},
                                {"value": "max", "name": "Max"}
                            ]
                        },
                        {
                            "id": "fast",
                            "name": "Fast",
                            "category": "model_config",
                            "type": "select",
                            "currentValue": "true",
                            "options": [
                                {"value": "false", "name": "Off"},
                                {"value": "true", "name": "Fast"}
                            ]
                        }
                    ]
                }
            ]
        }));

        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["auto", "gpt-5.4", "claude-opus-4-6"]
        );
        assert!(models[0].is_default);
        assert!(models[0].reasoning_efforts.is_empty());
        assert_eq!(
            models[1]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            models[1].default_reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(models[1].service_tiers[0].id, "fast");
        assert_eq!(models[1].default_service_tier.as_deref(), Some("default"));
        assert_eq!(
            models[1]
                .context_windows
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["272k", "1m"]
        );
        assert_eq!(models[1].default_context_window.as_deref(), Some("272k"));
        assert_eq!(
            models[2]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(models[2].default_reasoning_effort.as_deref(), Some("max"));
        assert_eq!(models[2].default_service_tier.as_deref(), Some("fast"));
    }

    #[test]
    fn parses_grok_default_and_available_models() {
        let models = parse_grok_models(
            "You are logged in with grok.com.\n\nDefault model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n  - my-custom-test\n",
        );
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "grok-4.6");
        assert!(models[0].is_default);
        assert_eq!(
            models[0]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(models[1].id, "grok-4.5");
        assert!(!models[1].is_default);
        assert_eq!(
            models[1]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high"]
        );
        assert_eq!(models[1].default_reasoning_effort.as_deref(), Some("high"));
        // A user-defined custom model keeps the picker entry but gets no
        // hardcoded reasoning menu; its effort support is not knowable from
        // the listing.
        assert_eq!(models[2].id, "my-custom-test");
        assert!(models[2].reasoning_efforts.is_empty());
        assert_eq!(models[2].default_reasoning_effort, None);
    }

    #[test]
    fn grok_fallback_catalog_is_empty() {
        // A fabricated fallback would offer a model the CLI rejects, so
        // discovery is authoritative and the pre-discovery picker is empty.
        assert!(fallback_models(ProviderKind::Grok).is_empty());
    }

    #[test]
    #[ignore = "requires an installed Kimi Code CLI"]
    fn installed_kimi_reports_models_and_a_default() {
        let binary =
            crate::command_env::find_executable("kimi").expect("Kimi Code CLI is not installed");
        let models = discover_kimi_models(&binary);
        assert!(
            !models.is_empty(),
            "the installed Kimi Code CLI reported no models"
        );
        assert!(
            models.iter().any(|model| model.is_default),
            "no Kimi model was marked as the configured default"
        );
    }

    #[test]
    fn parses_kimi_catalog_across_providers() {
        let catalog = json!({
            "providers": {"managed:kimi-code": {}, "moonshot-cn": {}},
            "models": {
                "kimi-code/k3": {
                    "provider": "managed:kimi-code",
                    "model": "k3",
                    "displayName": "K3",
                    "supportEfforts": ["low", "high", "max"],
                    "defaultEffort": "high"
                },
                "moonshot-cn/kimi-k2.6": {"provider": "moonshot-cn", "model": "kimi-k2.6"}
            }
        });

        let models = parse_kimi_models(&catalog, Some("moonshot-cn/kimi-k2.6"));

        assert_eq!(models.len(), 2);
        let k3 = &models[0];
        assert_eq!(k3.id, "kimi-code/k3");
        assert_eq!(k3.name, "K3");
        assert_eq!(k3.sub_provider.as_deref(), Some("Kimi Code"));
        assert!(!k3.is_default);
        assert_eq!(k3.reasoning_efforts.len(), 3);
        assert_eq!(k3.default_reasoning_effort.as_deref(), Some("high"));
        // No display name: the bare model id is the readable fallback, not the
        // provider-qualified alias.
        let k2 = &models[1];
        assert_eq!(k2.name, "Kimi K2.6");
        assert!(k2.is_default);
        assert!(k2.reasoning_efforts.is_empty());
    }

    #[test]
    fn reads_kimi_default_model_from_the_provider_listing() {
        assert_eq!(
            parse_kimi_default_model(
                "managed:kimi-code  type=kimi  models=4\n\nDefault model: kimi-code/k3\n"
            )
            .as_deref(),
            Some("kimi-code/k3")
        );
        assert!(parse_kimi_default_model("no default here").is_none());
    }

    #[test]
    fn parses_codex_model_list_metadata() {
        let response = json!({
            "id": 1,
            "result": {
                "data": [{
                    "model": "gpt-5.6-luna",
                    "displayName": "gpt-5.6-luna",
                    "isDefault": true,
                    "supportedReasoningEfforts": [
                        {"reasoningEffort": "low", "description": "Quick responses"},
                        {"reasoningEffort": "xhigh", "description": "Deep reasoning"}
                    ],
                    "defaultReasoningEffort": "xhigh",
                    "serviceTiers": [{
                        "id": "fast",
                        "name": "Fast",
                        "description": "Priority processing"
                    }],
                    "defaultServiceTier": "default"
                }]
            }
        });
        let models = parse_codex_model_response(&response);
        assert_eq!(models[0].name, "GPT-5.6-Luna");
        assert!(models[0].is_default);
        assert_eq!(models[0].reasoning_efforts[1].label, "Extra High");
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(models[0].service_tiers[0].id, "fast");
        assert_eq!(
            models[0].service_tiers[0].description.as_deref(),
            Some("Priority processing")
        );
    }

    #[test]
    fn parses_pi_models_and_model_specific_thinking_levels() {
        let state = json!({
            "id": "waku-state",
            "type": "response",
            "success": true,
            "data": {
                "model": {"provider": "github-copilot", "id": "gpt-5.6-terra"},
                "thinkingLevel": "xhigh"
            }
        });
        let response = json!({
            "id": "waku-models",
            "type": "response",
            "success": true,
            "data": {"models": [
                {
                    "provider": "github-copilot",
                    "id": "gpt-5.6-terra",
                    "name": "GPT-5.6 Terra",
                    "reasoning": true,
                    "thinkingLevelMap": {"off": null, "xhigh": "xhigh", "max": "max"}
                },
                {
                    "provider": "openai",
                    "id": "gpt-4o-mini",
                    "name": "GPT-4o mini",
                    "reasoning": false
                }
            ]}
        });

        let models = parse_pi_model_response(PiDialect::Pi, &state, &response);

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "github-copilot/gpt-5.6-terra");
        assert_eq!(models[0].sub_provider.as_deref(), Some("Github Copilot"));
        assert!(models[0].is_default);
        assert_eq!(
            models[0]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert!(models[1].reasoning_efforts.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn pi_rpc_discovery_keeps_model_provider_extensions_enabled() {
        let binary = write_fake_model_cli(
            "extensions",
            r#"#!/bin/sh
for argument in "$@"; do
  if [ "$argument" = "--no-extensions" ]; then
    exit 1
  fi
done
while IFS= read -r request; do
  case "$request" in
    *waku-models*)
      printf '%s\n' '{"id":"waku-models","type":"response","success":true,"data":{"models":[{"provider":"extension-provider","id":"extension-model","name":"Extension Model","reasoning":false}]}}'
      ;;
    *waku-state*)
      printf '%s\n' '{"id":"waku-state","type":"response","success":true,"data":{"model":{"provider":"extension-provider","id":"extension-model"},"thinkingLevel":"medium"}}'
      ;;
  esac
done
"#,
        );

        let models = discover_pi_models(&binary, PiDialect::Pi);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "extension-provider/extension-model");
        assert_eq!(models[0].name, "Extension Model");
        assert!(models[0].is_default);
        let _ = std::fs::remove_file(binary);
    }

    /// Oh My Pi replaced Pi's `thinkingLevelMap` with the list of efforts a
    /// model honors. `off` is never in that list but is always accepted.
    #[test]
    fn ohmypi_reasoning_options_come_from_the_advertised_efforts() {
        let state = json!({"data": {"model": {"provider": "deepseek", "id": "deepseek-v4-pro"}}});
        let response = json!({"data": {"models": [
            {
                "provider": "deepseek",
                "id": "deepseek-v4-pro",
                "name": "DeepSeek V4 Pro",
                "reasoning": true,
                "thinking": {"mode": "effort", "efforts": ["low", "high", "max"]}
            },
            {
                "provider": "deepseek",
                "id": "deepseek-v4-chat",
                "name": "DeepSeek V4 Chat",
                "reasoning": false
            }
        ]}});

        let models = parse_pi_model_response(PiDialect::OhMyPi, &state, &response);

        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["off", "low", "high", "max"]
        );
        assert!(models[1].reasoning_efforts.is_empty());
    }

    /// Ignored by default because it needs the CLI and its configured
    /// providers; run it after changing Oh My Pi's probe flags.
    #[test]
    #[ignore = "requires an installed, authenticated omp"]
    fn ohmypi_catalog_against_the_real_cli() {
        let binary = crate::command_env::find_executable("omp").expect("omp is not installed");
        let models = discover_pi_models(&binary, PiDialect::OhMyPi);
        assert!(!models.is_empty(), "Oh My Pi reported no models");
        for model in &models {
            assert!(
                model.id.contains('/'),
                "{} is not a provider/model slug",
                model.id
            );
            assert!(!model.name.trim().is_empty());
        }
        assert_eq!(
            models.iter().filter(|model| model.is_default).count(),
            1,
            "exactly one model should be marked default"
        );
        println!("discovered {} Oh My Pi models", models.len());
        for model in models.iter().take(5) {
            println!(
                "  {} — {} [{}]",
                model.id,
                model.name,
                model
                    .reasoning_efforts
                    .iter()
                    .map(|option| option.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
}

#[cfg(test)]
mod opencode_effort_smoke {
    /// Proves against the real CLI that the effort ladder is populated, which
    /// canned JSON cannot.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn discovers_reasoning_efforts_from_the_installed_cli() {
        let models = super::discover_opencode_models(std::path::Path::new("opencode"));
        let with_efforts: Vec<_> = models
            .iter()
            .filter(|model| !model.reasoning_efforts.is_empty())
            .collect();
        println!(
            "models={} with efforts={}",
            models.len(),
            with_efforts.len()
        );
        for model in with_efforts.iter().take(4) {
            println!(
                "  {} -> {:?} (default {:?})",
                model.id,
                model
                    .reasoning_efforts
                    .iter()
                    .map(|effort| effort.id.as_str())
                    .collect::<Vec<_>>(),
                model.default_reasoning_effort
            );
        }
        assert!(!models.is_empty(), "expected a catalogue");
        assert!(
            !with_efforts.is_empty(),
            "expected some models to expose variants"
        );
    }
}
