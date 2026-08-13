# Oh My Pi (OMP) 支持实现方案

> 基于对 Waku 源码与 OMP 17.2.15 真实二进制的协议实测（2026-08-12）。
> 目标：OMP 作为独立 provider（"Oh My Pi"，与 Pi 并列），skills 只扫 `.omp` 目录。

## 一、实测协议差异（证据：`omp --mode rpc` 真实交互）

### 兼容（无需改动）

| 项 | 实测结果 |
| --- | --- |
| ready 帧 | `{type:"ready", protocolVersion:1, supportedProtocolVersions:[1,2], maxFrameBytes:1048576, ...}`；Waku 按未知事件忽略，无碍 |
| `get_state` | `{id, type:"response", command:"get_state", success, data:{sessionId, model, thinkingLevel, ...}}`，字段与 Pi 一致 |
| `get_available_models` | `{data:{models:[...]}}`，Waku 的 `/data/models` 解析兼容；模型对象**无 `thinkingLevelMap`** → `pi_reasoning_options` 列出全部 7 档 effort，实测 `set_thinking_level` 对 off/minimal/low/medium/high/xhigh/max **全部接受** |
| `set_model` | 成功，响应 `data` 含 `contextWindow`（Waku 的 UsageUpdated 解析兼容） |
| `get_branch_messages` | `{data:{messages:[{entryId, text}]}}`，与 Pi `get_fork_messages` 结构一致 |
| `switch_session` | 带 id 响应，失败 `{success:false, error}` |
| `message_update` | `{type:"message_update", assistantMessageEvent:{type:"text_delta"|"thinking_delta", delta, partial}}` —— Waku `pi.rs:864` **本就解析 `assistantMessageEvent` 嵌套**，格式完全一致 |
| `message_start`/`message_end` | `/message/role`、`/message/usage` 与 Pi 一致 |
| `tool_execution_start/_update/_end` | 存在，字段 `toolCallId/toolName/args/partialResult/result/isError` 与 Waku 解析一致 |
| `auto_retry_end` | 存在，兼容 |
| `extension_ui_request` | OMP 启动推送 `setWidget`（autoresearch），method 不在 Waku 取消列表（select/confirm/input/editor）内，被忽略，无碍 |
| 未知命令 | 返回**无 id** 的 response，Waku 按 id 匹配会超时报错；Waku 只发已知命令，无碍 |

### 不兼容（必须适配）

| 项 | Pi | OMP | 适配 |
| --- | --- | --- | --- |
| 命令名 | `pi` | `omp` | ProviderKind 新增 |
| 启动参数 | `--mode rpc --approve` | `--mode rpc` + `--approval-mode <ask\|write\|yolo>`（`--approve` 报 unknown flag，exit 2） | 按 kind 分支 |
| 版本检查 | `PI_SKIP_VERSION_CHECK=1` | 无此机制（多余 env 无害） | 按 kind 分支 |
| 模型发现 flag | `--no-prompt-templates --no-context-files` 存在 | **不存在**，报 unknown flags，exit 2 | OMP 去掉这两个 flag |
| **turn 结束事件** | `agent_settled` → TurnFinished | **无 `agent_settled`**；turn 结束为 `turn_end`（agent 整体结束为 `agent_end`） | OMP 分支用 `turn_end` 触发 TurnFinished |
| rewind/branch | `get_fork_messages` → `fork {entryId}` / `clone` → `get_state` | `get_branch_messages` → `branch {entryId}`（`{data:{text, cancelled}}`）；**无 `clone`** | `fork_pi_session` 按 kind 分支；`turns_to_remove==0` 时 OMP 不支持（返回错误） |
| `session_info_changed` | 有（AutoTitle） | **无** | OMP 下标题更新缺失（次要，可接受） |
| skills/commands 目录 | `.pi/prompts`、`.pi/skills`、`~/.pi/agent/...` | `.omp/commands`、`.omp/skills`（项目，向上找最近祖先）、`~/.omp/agent/skills`（用户） | 按 kind 分支（只扫 `.omp`） |

### 权限映射（用户已确认）

| Waku RuntimeMode | OMP approvalMode | 启动参数 |
| --- | --- | --- |
| Supervised (Ask) | `always-ask` | `--approval-mode always-ask` |
| Auto-accept edits | `write` | `--approval-mode write` |
| Auto | `write`（折叠，保守） | `--approval-mode write` |
| Full access（默认） | `yolo`（OMP 默认） | 不传 |

`apply_options` 语义与 Pi 一致：权限仅启动时定，非默认组合返回 `false` 触发重启会话。

## 二、改动文件清单

| 文件 | 改动 |
| --- | --- |
| `src/model.rs` | `ProviderKind::OhMyPi`（id `"ohMyPi"`、display "Oh My Pi"、command `"omp"`、加入 `ALL` 与 `supports_conversation_rollback`）+ 测试 |
| `src/driver/pi.rs` | `PiDriver` 持有 `kind`；启动 args/env 分支；`agent_settled`/`turn_end` 分支；`fork_pi_session`/`pi_fork_request` → `branch` 适配 |
| `src/driver/mod.rs` | `ProviderKind::OhMyPi` 路由到 Pi 驱动 |
| `src/model_catalog.rs` | `discover_pi_models` 按 kind 传参 |
| `src/composer_complete.rs` | OMP 分支扫 `.omp/commands`、`.omp/skills`、`~/.omp/agent/skills` |
| `src/skills.rs` | OMP skill 目录 `.omp/skills`、`~/.omp/agent/skills` |
| `src/app/skills_page.rs` | `SkillSource::Provider(OhMyPi)` |
| `src/app/runtime.rs` | resume cursor / driver 生命周期分支 |
| `src/git_commit.rs` | commit 分支 |
| `src/ui/mod.rs` + `assets/` | provider 图标 `icons/provider-omp.svg` |
| `locales/` | provider 名、错误文案 |
| 文档 | `README.md`、`docs/providers.md`、`CHANGELOG.md` |

## 三、分步计划（约 6–8 人日）

1. ProviderKind 扩展（0.5d）
2. 驱动参数化：启动/事件/branch（2d）
3. 模型发现（0.5d）
4. skills/commands 目录（0.5–1d）
5. 其余模块接入（0.5–1d）
6. cargo check + 单测 + 手工回归（1–1.5d）
7. 文档（0.5d）

## 四、残余风险

- 真实会话（非 `--no-session`）的 `get_state` 是否返回 `sessionFile`（resume 依赖）——实现时实测确认（Pi 同名字段，低风险）。
- OMP 版本迭代快（17.2.15），协议可能继续漂移。
- computer-use 扩展（`pi-extension.ts`）对 OMP 的兼容性未验证，OMP 保留 `--extension` flag。
