//! 插件注册的 AI 工具：全局唯一名册与宿主→插件的调用桥。
//! 调用方向：宿主下发通知 `host:ai.toolCall`，插件经请求 `ai.toolResult` 回报结果。

use std::sync::atomic::Ordering;
use std::time::Duration;

use atelyx_plugin_protocol::method::RegisterToolParams;
use serde_json::Value;

use super::{PluginEvent, PluginRuntime};

/// 工具调用等待上限：插件无响应时按失败收场，不无限占住 Agent 的 run。
pub const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// 模型侧工具名的字符规则（字母 / 数字 / `_` / `-`，最长 64）。
pub(crate) fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 一个插件注册的 AI 工具（名称在内置工具与其他插件的工具之间全局唯一）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginToolSpec {
    pub plugin_id: String,
    /// 插件显示名（设置页清单展示用）。
    pub plugin_name: String,
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl PluginToolSpec {
    /// Agent 勾选清单里的工具 id（`plugin:<插件 id>:<工具名>`）。
    pub fn selection_id(&self) -> String {
        format!("plugin:{}:{}", self.plugin_id, self.name)
    }
}

/// 插件回传的一次工具结果。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginToolOutput {
    pub content: String,
    pub is_error: bool,
}

type ToolReplyTx = tokio::sync::oneshot::Sender<Result<PluginToolOutput, String>>;

/// 在途工具调用：发起插件 id + 结果回传通道。
pub(super) type ToolCallWait = (String, ToolReplyTx);

impl PluginRuntime {
    /// 注册一个工具。同一插件重复注册同名工具 = 原地更新描述与 schema；
    /// 与内置工具或其他插件重名、名称非法、描述为空、schema 非对象都报可读错误。
    pub(crate) fn register_tool(
        &self,
        plugin_id: &str,
        plugin_name: &str,
        params: RegisterToolParams,
    ) -> Result<(), String> {
        let name = params.name.trim();
        if !valid_tool_name(name) {
            return Err(format!(
                "工具名不合法（字母 / 数字 / _ / -，最长 64）：{}",
                params.name
            ));
        }
        let description = params.description.trim();
        if description.is_empty() {
            return Err("工具描述不能为空".to_string());
        }
        if !params.parameters.is_object() {
            return Err("工具参数 schema 必须是 JSON 对象".to_string());
        }
        if crate::ai::tools::builtin_tools().iter().any(|t| t.name == name) {
            return Err(format!("工具名与内置工具重名：{name}"));
        }
        {
            let mut tools = self.tools.lock().unwrap();
            if let Some(existing) = tools.get(name) {
                if existing.plugin_id != plugin_id {
                    return Err(format!("工具名已被插件 {} 占用：{name}", existing.plugin_id));
                }
            }
            tools.insert(
                name.to_string(),
                PluginToolSpec {
                    plugin_id: plugin_id.to_string(),
                    plugin_name: plugin_name.to_string(),
                    name: name.to_string(),
                    description: description.to_string(),
                    parameters: params.parameters,
                },
            );
        }
        let _ = self.events.send(PluginEvent::AiToolsChanged { id: plugin_id.to_string() });
        Ok(())
    }

    /// 撤销本插件注册的一个工具；未注册或不属于本插件都报可读错误。
    pub(crate) fn unregister_tool(&self, plugin_id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        {
            let mut tools = self.tools.lock().unwrap();
            match tools.get(name) {
                Some(spec) if spec.plugin_id == plugin_id => {}
                Some(_) => return Err(format!("工具不属于本插件：{name}")),
                None => return Err(format!("未注册的工具：{name}")),
            }
            tools.remove(name);
        }
        let _ = self.events.send(PluginEvent::AiToolsChanged { id: plugin_id.to_string() });
        Ok(())
    }

    /// 全部已注册工具（按插件 id + 工具名排序；插件停用即摘除）。
    pub fn plugin_tools(&self) -> Vec<PluginToolSpec> {
        let mut all: Vec<PluginToolSpec> = self.tools.lock().unwrap().values().cloned().collect();
        all.sort_by(|a, b| (&a.plugin_id, &a.name).cmp(&(&b.plugin_id, &b.name)));
        all
    }

    /// 摘除某插件的全部工具（插件停用 / 失败 / 崩溃 / 卸载时调用）。
    pub(crate) fn clear_tools_of(&self, plugin_id: &str) {
        let removed = {
            let mut tools = self.tools.lock().unwrap();
            let before = tools.len();
            tools.retain(|_, spec| spec.plugin_id != plugin_id);
            tools.len() != before
        };
        if removed {
            let _ = self.events.send(PluginEvent::AiToolsChanged { id: plugin_id.to_string() });
        }
    }

    /// 下发一次工具调用并等插件回报结果。插件未运行 / 未注册 / 超时 / 停用都以可读失败收场。
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<PluginToolOutput, String> {
        let spec = self
            .tools
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("工具未注册：{name}"))?;
        let instance = self
            .instances
            .lock()
            .unwrap()
            .get(&spec.plugin_id)
            .cloned()
            .ok_or_else(|| format!("插件未运行：{}", spec.plugin_id))?;
        let call_id = format!(
            "tool-{}",
            self.next_tool_call_id.fetch_add(1, Ordering::SeqCst)
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        instance.with_active_session(|| {
            self.tool_calls
                .lock()
                .unwrap()
                .insert(call_id.clone(), (spec.plugin_id.clone(), tx));
            instance.notify_tool_call(&call_id, &spec.name, arguments);
            Ok(())
        })?;
        match tokio::time::timeout(TOOL_CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("工具调用已取消".to_string()),
            Err(_) => {
                self.tool_calls.lock().unwrap().remove(&call_id);
                Err(format!(
                    "工具调用超时（{} 秒内未回传结果）：{name}",
                    TOOL_CALL_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// 插件回报一次工具调用结果。callId 不属于该插件或已摘除（超时 / 停用）时是空操作。
    pub(crate) fn resolve_tool_result(
        &self,
        plugin_id: &str,
        call_id: &str,
        content: String,
        is_error: bool,
    ) {
        let waiter = {
            let mut calls = self.tool_calls.lock().unwrap();
            match calls.get(call_id) {
                Some((owner, _)) if owner == plugin_id => calls.remove(call_id),
                _ => None,
            }
        };
        if let Some((_, tx)) = waiter {
            let _ = tx.send(Ok(PluginToolOutput { content, is_error }));
        }
    }

    /// 让某插件全部在途工具调用以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_tool_calls_of(&self, plugin_id: &str, reason: &str) {
        let waiters: Vec<ToolReplyTx> = {
            let mut calls = self.tool_calls.lock().unwrap();
            let ids: Vec<String> = calls
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| calls.remove(&id).map(|(_, tx)| tx))
                .collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 让全部在途工具调用以可读失败收场（宿主退出收尾）。
    pub(super) fn fail_all_tool_calls(&self, reason: &str) {
        let waiters: Vec<ToolReplyTx> = {
            let mut calls = self.tool_calls.lock().unwrap();
            calls.drain().map(|(_, (_, tx))| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }
}
