//! JSON-RPC 2.0 信封：一行一个 JSON（NDJSON），请求 / 通知 / 响应三态。
//! 字段名按 JSON-RPC 规范原文（jsonrpc/id/method/params/result/error），不做 camelCase 改写。
//! 解析唯一入口是 [`Envelope::parse`]；三个结构体只实现序列化（字段含 `&'static str`，
//! 生成 Deserialize 会要求输入也是 'static，对 owned 行文本不可用）。

use serde::{Deserialize, Serialize};
// serde_json::Error::custom 来自 serde::de::Error trait，需将该 trait 引入作用域
use serde::de::Error as _;
use serde_json::Value;

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
    /// 请求 id：本协议只认数字（发起方自增，单调不复用）。
    pub id: u64,
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
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// 一行信封的三种形态。解析失败给出可读原因（含行首片段），不静默丢弃。
///
/// 行为契约：`id` 必须是数字（字符串 id 与显式 `null` 都按形态非法拒绝）；响应携带 `params`
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
    pub fn request(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self::Request(Request { jsonrpc: Self::JSONRPC, id, method: method.into(), params })
    }

    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self::Notification(Notification { jsonrpc: Self::JSONRPC, method: method.into(), params })
    }

    pub fn success(id: u64, result: Value) -> Self {
        Self::Response(Response { jsonrpc: Self::JSONRPC, id, result: Some(result), error: None })
    }

    pub fn failure(id: u64, error: RpcError) -> Self {
        Self::Response(Response { jsonrpc: Self::JSONRPC, id, result: None, error: Some(error) })
    }

    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let snippet = line.chars().take(24).collect();
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            jsonrpc: String,
            /// 五个字段一律「出现即收下原值，再逐个判形态」，不用 `Option<T>` 直接承载：
            /// `Option<T>` 会把显式 `null` 读成 `None`，与「字段缺失」混为一谈——`id` / `method`
            /// 会因此绕过形态判定，`result` 则会把合法的 `"result": null` 判成缺字段。
            #[serde(default, deserialize_with = "present_value")]
            id: Option<Value>,
            #[serde(default, deserialize_with = "present_value")]
            method: Option<Value>,
            #[serde(default, deserialize_with = "present_value")]
            params: Option<Value>,
            #[serde(default, deserialize_with = "present_value")]
            result: Option<Value>,
            #[serde(default, deserialize_with = "present_value")]
            error: Option<Value>,
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
        let id = match raw.id {
            None => None,
            Some(Value::Null) => return Err(illegal("id 不得为 null")),
            Some(Value::Number(number)) => match number.as_u64() {
                Some(id) => Some(id),
                None => return Err(illegal("id 须为非负整数")),
            },
            Some(_) => return Err(illegal("id 须为数字")),
        };
        let method = match raw.method {
            None => None,
            Some(Value::String(method)) => Some(method),
            Some(_) => return Err(illegal("method 须为字符串")),
        };
        let error = match raw.error {
            None => None,
            Some(Value::Null) => return Err(illegal("error 不得为 null")),
            Some(value) => match serde_json::from_value::<RpcError>(value) {
                Ok(error) => Some(error),
                Err(e) => return Err(illegal(&format!("error 形态非法：{e}"))),
            },
        };
        match (id, method, raw.result, error) {
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
            // result 为 null 是合法的空结果；result 与 error 仍须二选一
            (Some(id), None, result, error) if result.is_some() ^ error.is_some() => {
                if raw.params.is_some() {
                    return Err(illegal("响应不得携带 params"));
                }
                Ok(Self::Response(Response { jsonrpc: Self::JSONRPC, id, result, error }))
            }
            _ => Err(illegal(
                "信封形态非法：请求须有数字 id + 字符串 method；响应须有 id 且 result/error 二选一；通知须有 method 且无 id",
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

/// 把「字段出现了」与「字段的值是什么」分开：字段存在即 `Some`（哪怕值是 null）。
fn present_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_three_forms() {
        let cases = [
            Envelope::request(1, "fs.read", serde_json::json!({"path": "/a"})),
            Envelope::notification("host:event", serde_json::json!({"name": "vault:switch"})),
            Envelope::success(7, serde_json::json!({"ok": true})),
            Envelope::failure(8, RpcError::permission_denied("未声明 fs")),
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
            // id 只认数字：字符串 id 与显式 null 都不是本协议的形态
            r#"{"jsonrpc":"2.0","id":"a-1","method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
        ] {
            assert!(Envelope::parse(bad).is_err(), "应拒绝：{bad}");
        }
    }

    /// 结果为 null 的响应必须能往返：宿主用 `success(id, Value::Null)` 应答无返回值的调用。
    #[test]
    fn null_result_roundtrips() {
        let line = Envelope::success(5, Value::Null).to_line();
        assert_eq!(line, r#"{"jsonrpc":"2.0","id":5,"result":null}"#);
        assert_eq!(Envelope::parse(&line).unwrap(), Envelope::success(5, Value::Null));
    }

    #[test]
    fn params_omitted_when_null() {
        let line = Envelope::notification("host:event", Value::Null).to_line();
        assert!(!line.contains("params"), "params 为 null 时应省略字段：{line}");
    }

    /// 解析不依赖输入的生命周期：owned 行文本必须能走通（流式读取的唯一形态）。
    #[test]
    fn parse_accepts_owned_lines() {
        let owned = format!("{}\n", Envelope::success(1, serde_json::json!({})).to_line());
        assert!(Envelope::parse(owned.trim_end()).is_ok());
    }
}
