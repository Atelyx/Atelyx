//! JSON-RPC 2.0 信封：一行一个 JSON（NDJSON），请求 / 通知 / 响应三态。
//! 字段名按 JSON-RPC 规范原文（jsonrpc/id/method/params/result/error），不做 camelCase 改写。
//! 解析唯一入口是 [`Envelope::parse`]；三个结构体只实现序列化（字段含 `&'static str`，
//! 生成 Deserialize 会要求输入也是 'static，对 owned 行文本不可用）。

use serde::{Deserialize, Serialize};
// serde_json::Error::custom 来自 serde::de::Error trait，需将该 trait 引入作用域
use serde::de::Error as _;
use serde_json::Value;

/// 请求 id：JSON-RPC 允许数字或字符串；本协议自己发起的请求一律用递增数字。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(u64),
    Str(String),
}

/// 错误码：-32700..-32603 为 JSON-RPC 规范保留段；-32001..-32099 为本协议自定义段。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// 能力未在 manifest 声明：fail-closed 拒绝，`message` 带可读原因。
    pub const PERMISSION_DENIED: i64 = -32001;
    /// 能力已声明但执行失败：`message` 为可读原因，`data` 可携带结构化细节。
    pub const FAILED: i64 = -32002;

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self { code: Self::PERMISSION_DENIED, message: message.into(), data: None }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self { code: Self::FAILED, message: message.into(), data: None }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code == Self::FAILED {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}（code {}）", self.message, self.code)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// 一行信封的三种形态。解析失败给出可读原因（含行首片段），不静默丢弃。
///
/// 行为契约：显式 `"id": null` 按通知解析（本协议不使用 null id）；响应携带 `params`
/// 视为形态非法拒绝；空行是否跳过由读循环决定（协议只承诺「空行解析必报错」）。
#[derive(Clone, Debug, PartialEq)]
pub enum Envelope {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

#[derive(Debug, thiserror::Error)]
#[error("信封解析失败（{snippet}）：{source}")]
pub struct ParseError {
    snippet: String,
    #[source]
    source: serde_json::Error,
}

impl Envelope {
    pub const JSONRPC: &'static str = "2.0";

    /// 构造插件→宿主（或宿主→插件，方法带 `host:` 前缀）的请求。
    pub fn request(id: RequestId, method: impl Into<String>, params: Value) -> Self {
        Self::Request(Request { jsonrpc: Self::JSONRPC, id, method: method.into(), params })
    }

    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self::Notification(Notification { jsonrpc: Self::JSONRPC, method: method.into(), params })
    }

    pub fn success(id: RequestId, result: Value) -> Self {
        Self::Response(Response { jsonrpc: Self::JSONRPC, id, result: Some(result), error: None })
    }

    pub fn failure(id: RequestId, error: RpcError) -> Self {
        Self::Response(Response { jsonrpc: Self::JSONRPC, id, result: None, error: Some(error) })
    }

    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let snippet = line.chars().take(24).collect();
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            jsonrpc: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            id: Option<RequestId>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            method: Option<String>,
            #[serde(default)]
            params: Option<Value>,
            #[serde(default)]
            result: Option<Value>,
            #[serde(default)]
            error: Option<RpcError>,
        }
        let raw: Raw = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(source) => return Err(ParseError { snippet, source }),
        };
        let illegal = |message: &str| {
            ParseError {
                snippet: snippet.clone(),
                source: serde_json::Error::custom(message.to_string()),
            }
        };
        if raw.jsonrpc != Self::JSONRPC {
            return Err(illegal(&format!("jsonrpc 字段必须为 \"2.0\"，实际 {:?}", raw.jsonrpc)));
        }
        match (raw.id, raw.method, raw.result, raw.error) {
            (Some(id), Some(method), None, None) => Ok(Self::Request(Request {
                jsonrpc: Self::JSONRPC,
                id,
                method,
                params: raw.params.unwrap_or(Value::Null),
            })),
            (None, Some(method), None, None) => Ok(Self::Notification(Notification {
                jsonrpc: Self::JSONRPC,
                method,
                params: raw.params.unwrap_or(Value::Null),
            })),
            (Some(id), None, result, error) if result.is_some() ^ error.is_some() => {
                if raw.params.is_some() {
                    return Err(illegal("响应不得携带 params"));
                }
                Ok(Self::Response(Response { jsonrpc: Self::JSONRPC, id, result, error }))
            }
            _ => Err(illegal(
                "信封形态非法：请求须有 id+method；响应须有 id 且 result/error 二选一；通知须有 method 且无 id",
            )),
        }
    }

    pub fn to_line(&self) -> String {
        let json = match self {
            Self::Request(r) => serde_json::to_string(r),
            Self::Notification(n) => serde_json::to_string(n),
            Self::Response(r) => serde_json::to_string(r),
        };
        // 自有类型的序列化不可能失败；失败即编程错误，panic 让它在测试期暴露
        json.expect("信封序列化失败")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_three_forms() {
        let cases = [
            Envelope::request(RequestId::Num(1), "fs.read", serde_json::json!({"path": "/a"})),
            Envelope::notification("host:event", serde_json::json!({"name": "vault:switch"})),
            Envelope::success(RequestId::Num(7), serde_json::json!({"ok": true})),
            Envelope::failure(RequestId::Str("x".into()), RpcError::permission_denied("未声明 fs")),
        ];
        for case in cases {
            let line = case.to_line();
            assert_eq!(Envelope::parse(&line).unwrap(), case, "roundtrip: {line}");
        }
    }

    #[test]
    fn parse_rejects_invalid_shapes() {
        for bad in [
            r#"{"jsonrpc":"1.0","id":1,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"m","result":{},"error":{"code":1,"message":"x"}}"#,
            r#"{"jsonrpc":"2.0","method":"m","id":1,"extra":true}"#,
            // 响应混入 params：形态非法，必须响亮拒绝
            r#"{"jsonrpc":"2.0","id":1,"result":{},"params":{}}"#,
        ] {
            assert!(Envelope::parse(bad).is_err(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn params_omitted_when_null() {
        let line = Envelope::notification("host:event", Value::Null).to_line();
        assert!(!line.contains("params"), "params 为 null 时应省略字段：{line}");
    }

    /// 解析不依赖输入的生命周期：owned 行文本必须能走通（流式读取的唯一形态）。
    #[test]
    fn parse_accepts_owned_lines() {
        let owned = format!("{}\n", Envelope::success(RequestId::Num(1), serde_json::json!({})).to_line());
        assert!(Envelope::parse(owned.trim_end()).is_ok());
    }
}
