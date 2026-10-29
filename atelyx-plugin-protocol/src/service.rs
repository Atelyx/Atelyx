//! 能力总线：能力按「服务 + 方法」登记，由提供者承载，调用方只认契约。
//!
//! 目录按方法全名的第一个 `.` 切出服务段与方法段；方法段自身还可分组（`ai.chat.send`）。
//! [`call`] 是「名字要到运行时才知道」时的信封形态，与直接调用同名方法落到同一实现。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 方法名常量：提供者登记与调用。
pub mod method {
    /// 提供者声明自己提供哪些服务方法（`{ service, version, methods }`）。登记不等于生效：
    /// 同一服务同时只有一个活动提供者，切换由用户决定（见 [`ServiceInfo::active`]）。
    pub const REGISTER: &str = "service.register";
    /// 撤销自己声明的服务（连带把它从候选中摘掉；活动提供者撤销后该服务回落内置实现）。
    pub const UNREGISTER: &str = "service.unregister";
    /// 枚举在册服务：服务名、版本、方法名、活动提供者与其余候选。
    pub const LIST: &str = "service.list";
    /// 调用某个服务的方法：`{ service, method, params }`，应答就是该方法自己的结果。
    pub const CALL: &str = "service.call";
    /// 宿主请求提供者执行一次调用（通知）：提供者用同一 `callId` 经 [`RESULT`] 回报。
    pub const HOST_CALL: &str = "host:service.call";
    /// 提供者回报一次调用的结果（`{ callId, result }` 或 `{ callId, error }`）。
    pub const RESULT: &str = "service.result";
    /// 活动的提供者变了（通知）：`service` 可用性变化时广播给全部运行中插件。
    pub const HOST_CHANGED: &str = "host:service.changed";
}

/// 服务版本的当前取值。契约不兼容的改动递增它，调用方据此判断能否沿用旧参数。
pub const SERVICE_VERSION: u32 = 1;

/// 服务名与方法名的字节上限（两者同口径）。
pub const MAX_METHOD_NAME: usize = 64;
/// 单个服务声明的方法数上限（`RegisterParams::methods` 的长度上限）。
pub const MAX_METHODS_PER_SERVICE: usize = 128;

/// `service.register` 的 params：声明本插件提供某个服务的若干方法。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    /// 服务名（与内置服务同名即成为它的候选提供者）。
    pub service: String,
    /// 契约版本；缺省 = [`SERVICE_VERSION`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// 本提供者实现的方法名（不含服务前缀；分组方法可自带 `.`，如 `chat.send`）。
    pub methods: Vec<String>,
}

impl RegisterParams {
    /// 校验服务名与方法名：非空、不含 `:`、长度受限、不重复。
    pub fn validate(&self) -> Result<(), String> {
        ensure_service_name(&self.service)?;
        if self.methods.is_empty() {
            return Err("至少声明一个方法".into());
        }
        if self.methods.len() > MAX_METHODS_PER_SERVICE {
            return Err(format!("单个服务的方法不得超过 {MAX_METHODS_PER_SERVICE} 个"));
        }
        let mut seen = std::collections::HashSet::new();
        for name in &self.methods {
            ensure_method_name(name)?;
            if !seen.insert(name.as_str()) {
                return Err(format!("方法名重复：{name}"));
            }
        }
        Ok(())
    }
}

fn ensure_method_name(name: &str) -> Result<(), String> {
    if name.trim() != name {
        return Err("方法名首尾不得有空白".into());
    }
    if name.len() > MAX_METHOD_NAME {
        return Err(format!("方法名不得超过 {MAX_METHOD_NAME} 字节"));
    }
    if name.split('.').any(str::is_empty) {
        return Err(format!("方法名不得为空或含空段：{name}"));
    }
    if name.contains(':') || name.chars().any(char::is_control) {
        return Err(format!("方法名不得含 `:` 或控制字符：{name}"));
    }
    Ok(())
}

fn ensure_service_name(name: &str) -> Result<(), String> {
    if name.trim() != name {
        return Err("服务名首尾不得有空白".into());
    }
    if name.is_empty() || name.len() > MAX_METHOD_NAME {
        return Err(format!("服务名不能为空且不得超过 {MAX_METHOD_NAME} 字节"));
    }
    if name.contains('.') || name.contains(':') || name.chars().any(char::is_control) {
        return Err(format!("服务名不得含 `.` / `:` 或控制字符：{name}"));
    }
    Ok(())
}

/// `service.unregister` 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub service: String,
}

/// 一个服务的当前状态（`service.list` 的条目）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub service: String,
    /// 活动提供者的契约版本。
    pub version: u32,
    /// 活动提供者实现的方法名。
    pub methods: Vec<String>,
    /// 活动提供者：`None` = 内置实现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 内置实现能否被替换（字体 / 主题这类平台自救面不可替换）。
    pub replaceable: bool,
    /// 其余候选提供者（插件 id，按登记序）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

/// `service.list` 的 result：按服务名排序。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub services: Vec<ServiceInfo>,
}

/// `service.call` 的 params。`params` 就是目标方法自己的入参（可为 null）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallParams {
    pub service: String,
    /// 方法名（不含服务前缀；分组方法自带 `.`，如 `chat.send`）。
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl CallParams {
    pub fn validate(&self) -> Result<(), String> {
        ensure_service_name(&self.service)?;
        ensure_method_name(&self.method)
    }

    /// 规范化后的完整方法名（`服务.方法`），与直接调用时的 wire 名一致。
    pub fn wire_name(&self) -> String {
        format!("{}.{}", self.service, self.method)
    }
}

/// `host:service.call` 的 params：宿主代调用方请求提供者执行一次调用。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCallParams {
    pub call_id: String,
    pub service: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// `service.result` 的 params：`result` 与 `error` 二选一（同时给出按错误处理）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultParams {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `host:service.changed` 的 params：活动提供者变化（含提供者下线）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedParams {
    pub service: String,
    /// 新的活动提供者：`None` = 内置实现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 该服务当前是否可用（提供者全不可用且无内置实现时 false）。
    pub available: bool,
}

/// 调用失败的可读原因（作为 JSON-RPC 错误的 data 回给调用方）。
pub mod error {
    /// 服务无人提供（既无内置实现，也没有活动的外部提供者）。
    pub const UNKNOWN_SERVICE: &str = "unknown_service";
    /// 活动提供者没有声明该方法。
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// 提供者在调用途中下线 / 停用 / 崩溃。
    pub const PROVIDER_UNAVAILABLE: &str = "provider_unavailable";
    /// 提供者超时未回报结果。
    pub const TIMEOUT: &str = "timeout";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_validates_names() {
        let ok = RegisterParams {
            service: "vault".into(),
            version: None,
            methods: vec!["readText".into(), "writeText".into()],
        };
        assert!(ok.validate().is_ok());

        let grouped = RegisterParams { methods: vec!["chat.send".into()], ..ok.clone() };
        assert!(grouped.validate().is_ok(), "分组方法自带 `.` 是合法方法名");

        let empty_segment = RegisterParams { methods: vec![".read".into()], ..ok.clone() };
        assert!(empty_segment.validate().is_err(), "方法名不得含空段");

        let blank = RegisterParams { methods: vec![String::new()], ..ok.clone() };
        assert!(blank.validate().is_err(), "空方法名不可登记");

        let dup = RegisterParams { methods: vec!["a".into(), "a".into()], ..ok.clone() };
        assert!(dup.validate().is_err(), "方法名须唯一");

        let empty = RegisterParams { methods: vec![], ..ok.clone() };
        assert!(empty.validate().is_err(), "至少声明一个方法");

        let bad_service = RegisterParams { service: "a:b".into(), ..ok };
        assert!(bad_service.validate().is_err(), "服务名不得含命名空间分隔符");
    }

    #[test]
    fn call_keeps_the_wire_shape() {
        let call = CallParams {
            service: "vault".into(),
            method: "readText".into(),
            params: serde_json::json!({ "path": "notes/a.md" }),
        };
        assert_eq!(call.wire_name(), "vault.readText");
        let wire = serde_json::to_value(&call).unwrap();
        assert_eq!(wire["service"], "vault");
        assert_eq!(wire["params"]["path"], "notes/a.md");
    }

    #[test]
    fn changed_payload_omits_the_builtin_provider() {
        let changed =
            ChangedParams { service: "markdown".into(), provider: None, available: true };
        let wire = serde_json::to_value(&changed).unwrap();
        assert!(wire.get("provider").is_none(), "内置实现不落 provider 字段");
        assert_eq!(wire["available"], true);
    }
}
