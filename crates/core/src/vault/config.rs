//! 仓库级配置与独立清单：config.json、prompt-notes、agents、folder-colors。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::atomic_write;
use super::chat_store::EditorChatModelOverride;

/// 供应商下的单个模型：id = API 请求用的模型名；nickname = 可选显示昵称（缺省 = id）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VaultModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

/// 仓库级 AI 供应商（磁盘格式，默认**不含 api_key**——key 走 keychain 条目
/// `provider-<sha256(root)>.<id>`，root = 仓库根（本地）或「服务器地址/空间 id」（空间）；
/// 仅 `syncKeys` 开启时随仓库落盘 apiKey，多设备同步）。运行时含 key 的 `ProviderConfig` 由调用方填充。
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VaultProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// 多模型列表（一个供应商多个模型，含可选昵称）。
    #[serde(default)]
    pub models: Vec<VaultModel>,
    /// API key（仅 syncKeys 开启时随仓库落盘/读取；关闭时调用方剥离不写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// 手写 Debug：key 字段只输出有无标记，不落明文（Debug 被日志/消息派生链引用）。
impl std::fmt::Debug for VaultProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultProvider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 仓库级搜索源配置（默认**不含 API key**——Tavily key 走 keychain 条目
/// `provider-<sha256(root)>.search-tavily`（root 口径同 VaultProvider）；仅 `syncKeys`
/// 开启时随仓库落盘 tavily_api_key）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct VaultSearchConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub searxng_url: String,
    /// Tavily API key（仅 syncKeys 开启时随仓库落盘/读取；关闭时调用方剥离不写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavily_api_key: Option<String>,
}

/// 手写 Debug：key 字段只输出有无标记，不落明文（Debug 被日志/消息派生链引用）。
impl std::fmt::Debug for VaultSearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultSearchConfig")
            .field("provider", &self.provider)
            .field("searxng_url", &self.searxng_url)
            .field("tavily_api_key", &self.tavily_api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 仓库级配置（默认**不含 API key**——key 走 keychain，条目按仓库隔离；
/// 仅 `syncKeys` 开启时 providers/search 落盘 key 字段，随仓库同步多设备）。
/// 主题/强调色/字号/字体/自动恢复开关为应用级（`commands/global.rs` 的 `GlobalConfig`）。
/// serde 类型层守边界：未开启时调用方剥离 key 字段再写盘；
/// `skip_serializing_if` 让无覆盖字段不落盘，保持 config.json 干净（{} = 无覆盖）。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VaultConfig {
    /// 仓库级默认模型（模型服务 tab 配置；缺省 = 未指定，跟随默认的对话请求报错提示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 仓库级默认模型所属供应商（与 model 配对固定供应商；缺省 = 调用方按 model 名反查首个命中，重选后落盘）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    /// 仓库级 AI 供应商列表（默认无 key；key 走 keychain 条目 `provider-<sha256(root)>.<id>`，
    /// `syncKeys` 开启时随仓库落盘 apiKey）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<VaultProvider>>,
    /// 仓库级搜索源配置（默认无 key；Tavily key 走 keychain 条目
    /// `provider-<sha256(root)>.search-tavily`，`syncKeys` 开启时随仓库落盘 tavily_api_key）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<VaultSearchConfig>,
    /// API key 是否随仓库保存（多设备同步）：开启后 key 明文落盘本文件随仓库同步；
    /// 缺省 false = key 仅存本机 keychain（按仓库隔离）。开启有泄露风险（仓库被公开/云盘共享）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_keys: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_explorer_sort: Option<String>,
    /// 文件面板排除的文件夹名（任何层级的同名文件夹不显示/不监听；设置页逗号分隔输入转数组）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_folders: Option<Vec<String>>,
    /// 附件导入默认文件夹（相对仓库根，可含子路径如 `assets/img`；缺省/空 = 仓库根目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_folder: Option<String>,
    /// 话题自动命名开关（缺省 true = 开启）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_naming_enabled: Option<bool>,
    /// 话题自动命名模型（缺省 = 跟随默认模型；指定后命名用该模型，话题命名一般用小模型）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_naming_model: Option<EditorChatModelOverride>,
    /// 启动仓库时自动切换到的场景 id（缺省 = 不切换，保持上次界面；切换场景恢复该场景记忆的激活布局）。
    /// 场景列表是应用级的（ui-state.json），id 悬挂（场景已删）由调用方解析守卫忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_scene_id: Option<String>,
    /// 画布对话产出自动落卡（正文 / 图片 / 写文件 → 卡并连血缘线；缺省 true = 开启）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_auto_materialize: Option<bool>,
}

/// 路径（相对 `.atelyx/`）与读写的唯一出口：命令层不自行拼路径，避免两处口径漂移。
const VAULT_CONFIG_FILE: &str = "config.json";

/// 读仓库级配置（文件不存在返回空覆盖）。
pub fn read_vault_config(root: &Path) -> Result<VaultConfig, String> {
    read_vault_config_with_backup(root).map(|(config, _)| config)
}

/// 读仓库级配置，并带回「损坏备份文件名」（`None` = 未发生损坏）。
///
/// 解析失败先备份原文再降级：仓库配置由调用方做「读 → 改 → 写」与字段级合并补丁，读到的空配置参与
/// 后续写盘——不备份就等于「一次外部编辑/磁盘异常静默清空供应商/默认模型」。
/// 备份文件名回传调用方：读到的空配置会让用户看到「设置全没了」，必须能告知原因与备份位置。
/// **备份失败即报错**（与补丁写路径同口径）：留不下原文还按空配置继续，调用方紧接着就会用
/// 空配置覆盖它——原文既没留档、用户也没被告知。
pub fn read_vault_config_with_backup(root: &Path) -> Result<(VaultConfig, Option<String>), String> {
    let path = root.join(".atelyx").join(VAULT_CONFIG_FILE);
    crate::config::read::<VaultConfig>(
        &path,
        "vault",
        crate::config::OnMissing::Default,
        crate::config::OnCorrupt::BackupDefault,
    )
}

pub fn write_vault_config(root: &Path, config: &VaultConfig) -> Result<(), String> {
    crate::config::write(&root.join(".atelyx").join(VAULT_CONFIG_FILE), config)
}

/// 删除「残留明文 key」的指令只在合并后仍不是同步模式时生效。
///
/// 该指令来自调用方**加载时**对文件的观察：
/// 期间文件可能已被别的设备改成 `syncKeys: true` 并写入合法 key，而调用方内存不会随之更新。只认磁盘事实——合并后仍是同步模式就不接受这次删键，防把别的设备刚写入的
/// key 抹掉；用户显式关闭开关时补丁同时带 `syncKeys: false`，删除照常生效。
fn guard_stale_key_deletion(base_json: &str, patch: &serde_json::Value) -> serde_json::Value {
    let Some(patch_obj) = patch.as_object() else {
        return patch.clone();
    };
    let deletes_key = patch_obj
        .get("search")
        .and_then(|s| s.as_object())
        .and_then(|s| s.get("tavilyApiKey"))
        .is_some_and(|v| v.is_null());
    if !deletes_key {
        return patch.clone();
    }
    let base: serde_json::Value = serde_json::from_str(base_json).unwrap_or(serde_json::json!({}));
    // 补丁里的 `syncKeys`：显式 `null` = 删键指令 = 合并后为默认 false（缺省才回落磁盘现值）
    let sync_after = match patch_obj.get("syncKeys") {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::Null) => false,
        _ => base
            .get("syncKeys")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    if !sync_after {
        return patch.clone();
    }
    let mut out = patch_obj.clone();
    if let Some(search) = out.get_mut("search").and_then(|s| s.as_object_mut()) {
        search.remove("tavilyApiKey");
    }
    serde_json::Value::Object(out)
}

/// 把字段级合并补丁应用到 `.atelyx/config.json`（读磁盘原文 → 合并 → 原子写），
/// 返回损坏原文的备份文件名（`None` = 未发生损坏；调用方据此提示用户）。
///
/// 为什么以磁盘原文为基线、而不以调用方内存态为基线：调用方各 setter 只发自己改动的字段，未出现在补丁里
/// 的字段必须保留**磁盘上的当前值**。撕裂窗口持独立 store 副本，其内存态可能落后于主窗口刚写入的值；
/// 若以内存态为基线整文件写，会把主窗口的改动整片抹掉（供应商丢失即此因）。
///
/// 基线损坏不得当空配置继续（否则只剩补丁字段写回、其余字段永久删除且不留档），故与读路径同口径
/// 先备份再按空基线合并；损坏判定走类型级反序列化（合法 JSON 但字段类型错同样会丢掉用户改动）。
pub fn patch_vault_config(root: &Path, patch: &serde_json::Value) -> Result<Option<String>, String> {
    let path = root.join(".atelyx").join(VAULT_CONFIG_FILE);
    // 读—合并—写整体持锁（同路径串行）：并发的两次补丁不会各自基于旧基线合并，谁都不会丢字段
    let lock = crate::config::lock(&path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (base, corrupt_backup) = crate::config::read::<VaultConfig>(
        &path,
        "vault",
        crate::config::OnMissing::Default,
        crate::config::OnCorrupt::BackupDefault,
    )?;
    let base_json = serde_json::to_string(&base).map_err(|e| e.to_string())?;
    let merged = merge_vault_config(&base_json, &guard_stale_key_deletion(&base_json, patch))?;
    write_vault_config(root, &merged)?;
    Ok(corrupt_backup)
}

/// 把字段级合并补丁应用到 config.json 的原始内容上（RFC 7386 语义：补丁值为 `null` 删键、
/// 值为对象递归合并、其余整体替换），返回合并后的配置。
///
/// 合并结果经 `VaultConfig` 反序列化再序列化：这一趟保证输出只含已知字段、保持 `skip_serializing_if`
/// 的干净形状（未知字段被丢弃，与 `write_vault_config` 同一形状约束）。
pub fn merge_vault_config(base_json: &str, patch: &serde_json::Value) -> Result<VaultConfig, String> {
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| "配置补丁必须是 JSON 对象".to_string())?;
    let mut base: serde_json::Value =
        serde_json::from_str(base_json).unwrap_or(serde_json::json!({}));
    if !base.is_object() {
        base = serde_json::json!({});
    }
    if let Some(target) = base.as_object_mut() {
        merge_json_objects(target, patch_obj);
    }
    serde_json::from_value(base).map_err(|e| format!("合并后的配置形状非法：{e}"))
}

/// 递归合并：`null` 删键，两侧同为对象则下钻，其余以补丁值覆盖。
pub fn merge_json_objects(
    target: &mut serde_json::Map<String, serde_json::Value>,
    patch: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
            continue;
        }
        // 两侧都是对象才下钻；数组与标量整体替换（`providers` 是列表，逐项合并无明确语义）
        let merged = match (target.get_mut(key), value) {
            (Some(serde_json::Value::Object(existing)), serde_json::Value::Object(incoming)) => {
                merge_json_objects(existing, incoming);
                None
            }
            _ => Some(value.clone()),
        };
        if let Some(v) = merged {
            target.insert(key.clone(), v);
        }
    }
}

// config.json 只保存仓库配置；标记列表单独落盘，避免混入配置字段。

/// 读系统提示词标记列表（不存在 = 空；损坏先备份原文再按空继续）。
/// 内容为相对仓库根的 `.md` 路径数组（如 `["笔记/提示词.md"]`）——本文件可重建，不因损坏阻塞进仓。
pub fn read_prompt_notes_file(root: &Path) -> Result<Vec<String>, String> {
    crate::config::read_or_default(&root.join(".atelyx").join("prompt-notes.json"), "prompt-notes")
}

/// 写系统提示词标记列表（原子写 .atelyx/prompt-notes.json）。
pub fn write_prompt_notes_file(root: &Path, files: &[String]) -> Result<(), String> {
    crate::config::write(&root.join(".atelyx").join("prompt-notes.json"), files)
}

// Agent = 可复用对话预设（名称 + 系统提示词 + 工具），对话节点/面板按 id 引用；
// 单独落盘避免混入配置字段（与 prompt-notes.json 同策略）。

/// agents.json 的 schema 版本（协作空间写盘同源引用，与本地写函数同一口径）。
pub const AGENTS_SCHEMA: &str = "atelyx-agents/v1";

/// 单条 Agent 配置（字段 camelCase）。
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    /// 引用已注册提示词笔记（相对仓库根 `.md` 路径，发送时实时读正文注入）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    /// 启用的工具 id 列表（空数组 = 不带工具）。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 预置标记（缺省 = 用户自建；预置 Agent 不可删除）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<bool>,
}

/// Agent 配置文件根结构（带 schema 便于后续演进）。
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AgentConfigFile {
    /// 缺省仍可解析：schema 只是演进标记，缺一段标记不足以否定一份结构完整的名单。
    #[serde(default)]
    pub schema: String,
    pub agents: Vec<AgentConfig>,
}

/// 内置「对话」Agent 固定 id（不可删除；新会话缺省选择）。
pub const BUILTIN_AGENT_CHAT_ID: &str = "builtin-chat";

/// 内置「Agent」Agent 固定 id（不可删除；全工具集，作自定义 Agent 的起点）。
pub const BUILTIN_AGENT_AGENT_ID: &str = "builtin-agent";

/// 内置 Agent 名册（顺序 = 列表/菜单展示顺序，恒置顶）：
/// 「对话」= 联网 + 只读工具（显式允许清单，防后续新增写入类工具漏进无写入语义的缺省 Agent）；
/// 「Agent」= 全部工具。可编辑（改名/改工具/配提示词），不可删除。
pub fn builtin_agents() -> Vec<AgentConfig> {
    vec![
        AgentConfig {
            id: BUILTIN_AGENT_CHAT_ID.to_string(),
            name: "对话".to_string(),
            system_prompt_file: None,
            tools: [
                "web_search",
                "web_fetch",
                "read_file",
                "glob",
                "grep",
                "list_dir",
                "read_history",
                "table_read",
                "canvas_read",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            builtin: Some(true),
        },
        AgentConfig {
            id: BUILTIN_AGENT_AGENT_ID.to_string(),
            name: "Agent".to_string(),
            system_prompt_file: None,
            tools: [
                "web_search",
                "web_fetch",
                "read_file",
                "glob",
                "grep",
                "list_dir",
                "read_history",
                "write_file",
                "append_file",
                "edit_file",
                "move",
                "delete",
                "table_read",
                "table_edit",
                "canvas_read",
                "canvas_edit",
                "todo_write",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            builtin: Some(true),
        },
    ]
}

/// 名册缺失的内置 Agent 前置补齐（幂等；已有同 id 条目不动，保用户编辑）。
pub fn ensure_builtin_agents(agents: Vec<AgentConfig>) -> Vec<AgentConfig> {
    let mut merged: Vec<AgentConfig> = builtin_agents()
        .into_iter()
        .filter(|b| !agents.iter().any(|a| a.id == b.id))
        .collect();
    merged.extend(agents);
    merged
}

/// 工具 id 迁移：旧工具名映射到当前名（去重保序），写回只写当前名。
/// 旧 id 分别对应「移动 / 重命名」与「删文件 / 删目录」两种意图，迁移后由同一工具承载。
pub fn migrate_tool_ids(tools: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tools.len());
    for id in tools {
        let id = match id.as_str() {
            "rename_file" | "move_file" => "move",
            "delete_file" | "delete_dir" => "delete",
            other => other,
        }
        .to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// 内置 Agent 的历史默认工具集（迁移判据：工具集完全等于它 = 用户未改过，可整体换成当前默认）。
fn legacy_builtin_tools(id: &str) -> Option<&'static [&'static str]> {
    match id {
        BUILTIN_AGENT_CHAT_ID => Some(&[
            "web_search",
            "web_fetch",
            "read_file",
            "glob",
            "grep",
            "list_dir",
            "read_history",
        ]),
        BUILTIN_AGENT_AGENT_ID => Some(&[
            "web_search",
            "web_fetch",
            "read_file",
            "glob",
            "grep",
            "list_dir",
            "read_history",
            "write_file",
            "append_file",
            "edit_file",
            "move",
            "delete",
            "todo_write",
        ]),
        _ => None,
    }
}

/// Agent 配置装载迁移（本地与空间同口径）：工具 id 归一 + 未改过的内置 Agent 换成当前默认集。
/// 用户改过的内置 Agent 与自建 Agent 的工具集不动。
pub fn migrate_agent_configs(agents: &mut [AgentConfig]) {
    let defaults = builtin_agents();
    for agent in agents.iter_mut() {
        agent.tools = migrate_tool_ids(std::mem::take(&mut agent.tools));
        let (Some(default), Some(legacy)) =
            (defaults.iter().find(|d| d.id == agent.id), legacy_builtin_tools(&agent.id))
        else {
            continue;
        };
        let legacy: Vec<String> = legacy.iter().map(|s| s.to_string()).collect();
        if agent.tools == legacy {
            agent.tools = default.tools.clone();
        }
    }
}

/// 解析 agents.json 正文（本地与空间同口径）：严格解析 + 工具 id / 内置默认集迁移。
///
/// 解析失败必须是错误而不是空名单：名单走「读 → 改 → 写」整文件覆盖，把损坏读成空
/// 会让下一次保存把用户的全部 Agent 抹掉。缺 `schema` 不算损坏（见 `AgentConfigFile`）。
pub fn parse_agents_json(raw: &str) -> Result<Vec<AgentConfig>, String> {
    let file: AgentConfigFile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut agents = file.agents;
    migrate_agent_configs(&mut agents);
    Ok(agents)
}

/// 读 Agent 配置列表：文件不存在 = 空名单；内容损坏 = 备份原文后返回错误（不降级为空名单）。
///
/// 备份失败同样报错：留不下原文还继续，调用方紧接着的保存就会把它永久覆盖。
pub fn read_agents_file(root: &Path) -> Result<Vec<AgentConfig>, String> {
    let path = root.join(".atelyx").join("agents.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    match parse_agents_json(&json) {
        Ok(agents) => Ok(agents),
        Err(e) => {
            let Some(backup) = crate::config::backup(&path, "agents") else {
                return Err(format!(
                    "Agent 配置已损坏且原文备份失败（{}），已中止读取以免覆盖原文：{e}",
                    path.display()
                ));
            };
            Err(format!(
                "Agent 配置已损坏，原文已备份为 {}：{e}",
                backup.display()
            ))
        }
    }
}

/// 写 Agent 配置列表（原子写 .atelyx/agents.json）。
pub fn write_agents_file(root: &Path, agents: &[AgentConfig]) -> Result<(), String> {
    let path = root.join(".atelyx").join("agents.json");
    let file = AgentConfigFile {
        schema: AGENTS_SCHEMA.to_string(),
        agents: agents.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    atomic_write(&path, &json)
}

// config.json 只保存仓库配置；文件夹颜色单独落盘，避免混入配置字段（与 prompt-notes.json 同策略）。

/// 读文件夹图标颜色映射（相对仓库根路径 → hex 色；不存在 = 空；损坏先备份原文再按空继续）。
pub fn read_folder_colors_file(root: &Path) -> Result<HashMap<String, String>, String> {
    crate::config::read_or_default(&root.join(".atelyx").join("folder-colors.json"), "folder-colors")
}

/// 写文件夹图标颜色映射（原子写 .atelyx/folder-colors.json）。
pub fn write_folder_colors_file(
    root: &Path,
    colors: &HashMap<String, String>,
) -> Result<(), String> {
    crate::config::write(&root.join(".atelyx").join("folder-colors.json"), colors)
}

#[cfg(test)]
mod merge_vault_config_tests {
    use crate::vault::test_support::TempDir;
    use std::path::PathBuf;

    use super::*;

    fn apply(base: &str, patch: &str) -> VaultConfig {
        let patch: serde_json::Value = serde_json::from_str(patch).unwrap();
        merge_vault_config(base, &patch).unwrap()
    }

    #[test]
    fn only_patched_fields_change() {
        // 撕裂窗口场景：补丁只带排序，磁盘上的供应商必须原样保留
        let merged = apply(
            r#"{"providers":[{"id":"p1","name":"A","baseUrl":"u","models":[]}],"fileExplorerSort":"name"}"#,
            r#"{"fileExplorerSort":"updated"}"#,
        );
        assert_eq!(merged.file_explorer_sort.as_deref(), Some("updated"));
        assert_eq!(merged.providers.unwrap()[0].id, "p1");
    }

    #[test]
    fn null_removes_key() {
        let merged = apply(r#"{"model":"m1","modelProviderId":"p1"}"#, r#"{"model":null}"#);
        assert!(merged.model.is_none());
        assert_eq!(merged.model_provider_id.as_deref(), Some("p1"));
    }

    #[test]
    fn nested_object_merges_and_null_removes_only_its_key() {
        let merged = apply(
            r#"{"search":{"provider":"tavily","searxngUrl":"http://x"}}"#,
            r#"{"search":{"searxngUrl":null,"provider":"searxng"}}"#,
        );
        let search = merged.search.unwrap();
        assert_eq!(search.provider, "searxng");
        assert_eq!(search.searxng_url, "");
    }

    #[test]
    fn array_replaces_whole_not_item_wise() {
        let merged = apply(
            r#"{"providers":[{"id":"p1","name":"A","baseUrl":"u","models":[]},{"id":"p2","name":"B","baseUrl":"u","models":[]}]}"#,
            r#"{"providers":[{"id":"p3","name":"C","baseUrl":"u","models":[]}]}"#,
        );
        let providers = merged.providers.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "p3");
    }

    #[test]
    fn unknown_patch_fields_are_dropped() {
        let merged = apply(r#"{"model":"m1"}"#, r#"{"不认识的字段":1,"model":"m2"}"#);
        assert_eq!(merged.model.as_deref(), Some("m2"));
        let json = serde_json::to_string(&merged).unwrap();
        assert!(!json.contains("不认识的字段"));
    }

    #[test]
    fn unknown_base_fields_and_corrupt_base_are_tolerated() {
        let merged = apply(r#"{"未来字段":1,"model":"m1"}"#, r#"{"autoNamingEnabled":true}"#);
        assert_eq!(merged.model.as_deref(), Some("m1"));
        assert_eq!(merged.auto_naming_enabled, Some(true));

        // 布尔开关按 camelCase 键落盘，且补丁只动自身字段（其余字段经合并保留）
        let merged = apply(r#"{"autoNamingEnabled":false}"#, r#"{"syncKeys":true}"#);
        assert_eq!(merged.sync_keys, Some(true));
        assert_eq!(merged.auto_naming_enabled, Some(false));

        let merged = apply("这不是 JSON", r#"{"model":"m1"}"#);
        assert_eq!(merged.model.as_deref(), Some("m1"));
    }

    #[test]
    fn non_object_patch_is_rejected() {
        let patch: serde_json::Value = serde_json::from_str("[1,2]").unwrap();
        assert!(merge_vault_config("{}", &patch).is_err());
    }

    #[test]
    fn patch_round_trips_through_disk() {
        let root = TempDir::new("vault-config-patch");
        std::fs::create_dir_all(root.join(".atelyx")).unwrap();
        std::fs::write(
            root.join(".atelyx").join("config.json"),
            r#"{"providers":[{"id":"p1","name":"A","baseUrl":"u","models":[]}],"fileExplorerSort":"name"}"#,
        )
        .unwrap();
        let patch: serde_json::Value = serde_json::from_str(r#"{"attachmentFolder":"附件"}"#).unwrap();
        let backup = patch_vault_config(&root, &patch).unwrap();
        assert!(backup.is_none(), "配置正常时不应报告损坏");
        let on_disk = read_vault_config(&root).unwrap();
        assert_eq!(on_disk.attachment_folder.as_deref(), Some("附件"));
        assert_eq!(on_disk.providers.unwrap()[0].id, "p1");
        assert_eq!(on_disk.file_explorer_sort.as_deref(), Some("name"));
    }

    #[test]
    fn concurrent_patches_keep_every_field() {
        // 补丁是「读—合并—写」：并发写同一配置文件时若不串行，后写者会用自己的旧基线覆盖先写者的字段。
        // 每个线程改一个不同字段，全部字段都必须留下。
        let root = TempDir::new("vault-config-patch-concurrent");
        std::fs::create_dir_all(root.join(".atelyx")).unwrap();
        let patches: Vec<serde_json::Value> = [
            r#"{"model":"m1"}"#,
            r#"{"modelProviderId":"p1"}"#,
            r#"{"attachmentFolder":"附件"}"#,
            r#"{"excludeFolders":["草稿"]}"#,
            r#"{"fileExplorerSort":"name"}"#,
            r#"{"syncKeys":true}"#,
            r#"{"autoNamingEnabled":true}"#,
            r#"{"search":{"provider":"searxng","searxngUrl":"http://x"}}"#,
        ]
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
        let handles: Vec<_> = patches
            .into_iter()
            .map(|patch| {
                let dir: PathBuf = root.to_path_buf();
                std::thread::spawn(move || patch_vault_config(&dir, &patch).unwrap())
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let on_disk = read_vault_config(&root).unwrap();
        assert_eq!(on_disk.model.as_deref(), Some("m1"));
        assert_eq!(on_disk.model_provider_id.as_deref(), Some("p1"));
        assert_eq!(on_disk.attachment_folder.as_deref(), Some("附件"));
        assert_eq!(on_disk.exclude_folders.unwrap(), vec!["草稿".to_string()]);
        assert_eq!(on_disk.file_explorer_sort.as_deref(), Some("name"));
        assert_eq!(on_disk.sync_keys, Some(true));
        assert_eq!(on_disk.auto_naming_enabled, Some(true));
        assert_eq!(on_disk.search.unwrap().searxng_url, "http://x");
    }

    #[test]
    fn patch_reports_corrupt_base_and_keeps_backup() {        let root = TempDir::new("vault-config-patch-corrupt");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{ 这不是 JSON").unwrap();

        let patch: serde_json::Value = serde_json::from_str(r#"{"attachmentFolder":"附件"}"#).unwrap();
        let backup = patch_vault_config(&root, &patch).unwrap().expect("损坏必须回传备份名");

        // 原文留档 + 备份名可定位；补丁按空基线写回（其余字段只在备份里）
        assert!(dir.join(&backup).is_file());
        let on_disk = read_vault_config(&root).unwrap();
        assert_eq!(on_disk.attachment_folder.as_deref(), Some("附件"));
    }

    #[test]
    fn patch_treats_wrong_field_types_as_corrupt() {
        // 合法 JSON 但字段类型错：不先留档就合并会失败，用户改动静默丢失且无从取回
        let root = TempDir::new("vault-config-patch-typemismatch");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"providers":"oops"}"#).unwrap();

        let patch: serde_json::Value = serde_json::from_str(r#"{"attachmentFolder":"附件"}"#).unwrap();
        let backup = patch_vault_config(&root, &patch).unwrap().expect("类型不符按损坏处理");
        assert_eq!(
            std::fs::read_to_string(dir.join(&backup)).unwrap(),
            r#"{"providers":"oops"}"#
        );
    }

    #[test]
    fn stale_key_deletion_is_ignored_while_sync_keys_on_disk() {
        // 删键指令来自调用方加载时的观察；磁盘已被别的设备改成 syncKeys=true 时不得生效
        //（否则会把另一设备刚写入的合法 key 抹掉）
        let root = TempDir::new("vault-config-stale-key");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"syncKeys":true,"search":{"provider":"tavily","tavilyApiKey":"tvly-new"}}"#,
        )
        .unwrap();
        let patch: serde_json::Value =
            serde_json::from_str(r#"{"search":{"tavilyApiKey":null}}"#).unwrap();
        patch_vault_config(&root, &patch).unwrap();
        let on_disk = read_vault_config(&root).unwrap();
        assert_eq!(
            on_disk.search.as_ref().and_then(|s| s.tavily_api_key.as_deref()),
            Some("tvly-new"),
            "磁盘仍是同步模式时不得执行残留 key 删键"
        );

        // 用户显式关闭开关（补丁同时带 syncKeys=false）：删除照常生效
        let patch: serde_json::Value = serde_json::from_str(
            r#"{"syncKeys":false,"search":{"provider":"tavily","tavilyApiKey":null}}"#,
        )
        .unwrap();
        patch_vault_config(&root, &patch).unwrap();
        let on_disk = read_vault_config(&root).unwrap();
        assert!(on_disk.search.as_ref().and_then(|s| s.tavily_api_key.as_ref()).is_none());

        // 磁盘本就不是同步模式：删除照常生效
        std::fs::write(
            &path,
            r#"{"search":{"provider":"tavily","tavilyApiKey":"tvly-stray"}}"#,
        )
        .unwrap();
        let patch: serde_json::Value =
            serde_json::from_str(r#"{"search":{"tavilyApiKey":null}}"#).unwrap();
        patch_vault_config(&root, &patch).unwrap();
        let on_disk = read_vault_config(&root).unwrap();
        assert!(on_disk.search.as_ref().and_then(|s| s.tavily_api_key.as_ref()).is_none());
    }

    #[test]
    fn corrupt_config_is_backed_up_not_silently_dropped() {
        let root = TempDir::new("vault-config-corrupt");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ 这不是 JSON").unwrap();

        let (config, backup) = read_vault_config_with_backup(&root).unwrap();

        assert!(config.providers.is_none(), "损坏配置降级为空覆盖");
        // 原文必须留档：否则紧接的写盘会把它永久抹掉
        let backups: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("config.json.corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "应有且只有一个备份：{backups:?}");
        // 备份文件名必须回传调用方：读到的空配置要能向用户说明原因（否则「设置与 key 全没了」无从解释）
        assert_eq!(backup.as_deref(), Some(backups[0].as_str()));
        assert_eq!(
            std::fs::read_to_string(dir.join(&backups[0])).unwrap(),
            "{ 这不是 JSON"
        );
        // 原路径已让开：后续写盘不会与备份互相覆盖
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod agents_file_tests {
    use crate::vault::test_support::TempDir;
    use super::*;

    fn corrupt_backups(root: &Path) -> Vec<String> {
        std::fs::read_dir(root.join(".atelyx"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("agents.json.corrupt-"))
            .collect()
    }

    fn write_raw(root: &Path, raw: &str) {
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.json"), raw).unwrap();
    }

    #[test]
    fn missing_file_reads_as_empty_without_creating_it() {
        let root = TempDir::new("agents-missing");
        assert!(read_agents_file(&root).unwrap().is_empty());
        assert!(!root.join(".atelyx/agents.json").exists(), "读不落盘");
    }

    /// 缺 schema 的名单仍可解析：否则旧版/手编辑文件会被判损坏，进而被清空。
    #[test]
    fn missing_schema_still_parses() {
        let root = TempDir::new("agents-noschema");
        write_raw(
            &root,
            r#"{"agents":[{"id":"a","name":"A","tools":["read_file"]}]}"#,
        );
        let agents = read_agents_file(&root).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "a");
        assert_eq!(agents[0].tools, vec!["read_file".to_string()]);
        assert!(corrupt_backups(&root).is_empty(), "可解析的文件不当损坏处理");
    }

    /// 损坏 = 备份原文 + 报错；不得降级为空名单（否则下一次保存把名单写成空）。
    #[test]
    fn corrupt_file_is_backed_up_and_reported() {
        let root = TempDir::new("agents-corrupt");
        write_raw(&root, "{ 这不是 JSON");
        let path = root.join(".atelyx/agents.json");

        let err = read_agents_file(&root).unwrap_err();
        assert!(err.contains("已备份"), "{err}");
        assert!(!path.exists(), "损坏原文已让开，后续写盘不会覆盖它");
        let backups = corrupt_backups(&root);
        assert_eq!(backups.len(), 1, "应且只应留下一个备份：{backups:?}");
        assert_eq!(
            std::fs::read_to_string(root.join(".atelyx").join(&backups[0])).unwrap(),
            "{ 这不是 JSON",
            "备份逐字保留原文"
        );
        // 备份后原路径为空：后续读取按「尚无配置」放行，用户已从错误里知道原文位置
        assert!(read_agents_file(&root).unwrap().is_empty());
        assert_eq!(corrupt_backups(&root).len(), 1, "不再重复备份同一份内容");
    }

    /// 空文件不是「名单为空」，与损坏同处理：否则一次误触清空会连带清掉全部 Agent。
    #[test]
    fn empty_file_is_corrupt_not_empty_roster() {
        let root = TempDir::new("agents-empty");
        write_raw(&root, "");
        assert!(read_agents_file(&root).is_err());
        let backups = corrupt_backups(&root);
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(root.join(".atelyx").join(&backups[0])).unwrap(),
            "",
            "空文件原样留档"
        );
    }

    #[test]
    fn write_then_read_roundtrip_keeps_agents() {
        let root = TempDir::new("agents-roundtrip");
        let agents = vec![
            AgentConfig {
                id: "a".into(),
                name: "A".into(),
                system_prompt_file: Some("笔记/提示词.md".into()),
                tools: vec!["read_file".into(), "grep".into()],
                builtin: None,
            },
            AgentConfig {
                id: BUILTIN_AGENT_CHAT_ID.into(),
                name: "对话".into(),
                system_prompt_file: None,
                tools: vec!["read_file".into()],
                builtin: Some(true),
            },
        ];
        write_agents_file(&root, &agents).unwrap();
        let raw = std::fs::read_to_string(root.join(".atelyx/agents.json")).unwrap();
        assert!(raw.contains(AGENTS_SCHEMA), "写盘带 schema：{raw}");
        assert_eq!(read_agents_file(&root).unwrap(), agents);
        assert!(corrupt_backups(&root).is_empty());
    }

    /// 解析口径：数组根、缺 agents 字段都算损坏（不是空名单）。
    #[test]
    fn structurally_wrong_json_is_an_error() {
        let root = TempDir::new("agents-shape");
        write_raw(&root, "[]");
        assert!(read_agents_file(&root).is_err());
        write_raw(&root, r#"{"schema":"atelyx-agents/v1"}"#);
        assert!(read_agents_file(&root).is_err());
    }
}

#[cfg(test)]
mod config_files_tests {
    use crate::vault::read_editor_chats_meta_file;
    use crate::vault::test_support::TempDir;
    use super::*;

    fn backups(root: &Path, name: &str) -> Vec<String> {
        std::fs::read_dir(root.join(".atelyx"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("{name}.corrupt-")))
            .collect()
    }

    /// 可重建的配置清单损坏时必须留档：读成空再被下一次保存写回，等于静默清空用户设置。
    #[test]
    fn corrupt_roster_files_are_backed_up_not_silently_emptied() {
        let root = TempDir::new("cfg-roster-corrupt");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("prompt-notes.json"), "{ 坏").unwrap();
        assert!(read_prompt_notes_file(&root).unwrap().is_empty());
        assert_eq!(backups(&root, "prompt-notes.json").len(), 1);
        assert!(!dir.join("prompt-notes.json").exists(), "原文已让开");

        std::fs::write(dir.join("folder-colors.json"), "不是 JSON").unwrap();
        assert!(read_folder_colors_file(&root).unwrap().is_empty());
        assert_eq!(backups(&root, "folder-colors.json").len(), 1);

        std::fs::write(dir.join("editor-chats-meta.json"), "{\"schema\":").unwrap();
        let meta = read_editor_chats_meta_file(&root).unwrap();
        assert!(meta.model_override.is_none());
        assert_eq!(backups(&root, "editor-chats-meta.json").len(), 1);
    }

    /// schema 标记缺失的文件仍在口径内：只更新 schema 版本不算损坏，不产生备份。
    #[test]
    fn editor_meta_without_schema_is_not_corrupt() {
        let root = TempDir::new("cfg-editor-meta-noschema");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("editor-chats-meta.json"), r#"{"agentOverride":"a"}"#).unwrap();
        let meta = read_editor_chats_meta_file(&root).unwrap();
        // 缺 schema 按空覆盖处理（与 schema 不符同视），但不留备份
        assert!(meta.agent_override.is_none());
        assert!(backups(&root, "editor-chats-meta.json").is_empty());
    }
}
