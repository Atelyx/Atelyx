//! 协议编码对拍：快照（`fixtures/plugin-protocol/protocol.json`）= 唯一期望源。
//! 信封行冻结逐字节编码；整面帧与握手冻结字段形状。改协议 = 显式改快照并升版本。

use atelyx_plugin_protocol::envelope::Envelope;
use atelyx_plugin_protocol::event::{
    ChatChangedPayload, FileChangedPayload, TableChangedPayload, VaultSwitchPayload,
};
use atelyx_plugin_protocol::handshake::{self, InitializeParams, InitializeResult};
use atelyx_plugin_protocol::frame::FrameSubmitParams;
use atelyx_plugin_protocol::method::{
    ChatHistoryParams, ChatRenameParams, ChatSendParams, ChatSessionParams, CollabMessageParams,
    CollabPeersResult, CollabSendParams, CollabSendResult, DocCloseParams, DocOpenParams,
    HttpRequestParams, HttpResult, ImageInfoResult, ImportFileParams, ListResult,
    MeasureTextResult, PathParams, PickFileParams, PickedPath, ProcessEventParams, RegisterParams,
    RegisterShortcutParams, RegisterToolParams, SetTrayItemsParams, ShortcutQueryParams,
    ShortcutQueryResult, ShowNotificationParams, SurfaceAttachedParams, TableAddRowResult,
    TableRowIdParams, TableSetCellImageParams, TableSnapshot, TableUpdateCellParams, TakeoverParams,
    TextPayload, ToolCallParams, ToolResultParams, TrayClickParams, UnregisterParams,
    UnregisterToolParams, WindowCloseParams, WindowClosedParams, WindowIdParams, WindowOpenParams,
    WindowUpdateParams,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/plugin-protocol/protocol.json")
}

fn load() -> Value {
    let raw = fs::read_to_string(fixture_path())
        .expect("fixtures/plugin-protocol/protocol.json 缺失：对拍快照应随源码入库");
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn envelope_lines_are_frozen() {
    let snapshot = load();
    let cases = snapshot["envelopes"].as_array().expect("快照缺 envelopes 数组");
    assert!(cases.len() >= 5, "语料规模兜底：快照疑似被清空");

    for case in cases {
        let name = case["name"].as_str().unwrap_or_default();
        let line = case["line"].as_str().unwrap_or_default();
        let parsed = Envelope::parse(line).unwrap_or_else(|e| panic!("语料 {name} 解析失败：{e}"));
        assert_eq!(parsed.to_line(), line, "语料 {name}: 编码与快照不一致");

        let expected_kind = case["kind"].as_str().unwrap_or_default();
        let actual_method = match &parsed {
            Envelope::Request(r) => {
                assert_eq!(expected_kind, "request", "语料 {name}: kind 不符");
                r.method.as_str()
            }
            Envelope::Notification(n) => {
                assert_eq!(expected_kind, "notification", "语料 {name}: kind 不符");
                n.method.as_str()
            }
            Envelope::Response(_) => {
                assert_eq!(expected_kind, "response", "语料 {name}: kind 不符");
                ""
            }
        };
        match case["method"].as_str() {
            Some(expected) => assert_eq!(actual_method, expected, "语料 {name}: method 不符"),
            None => assert_eq!(actual_method, "", "语料 {name}: 不应有 method"),
        }
    }
}

#[test]
fn frame_shape_is_frozen() {
    let snapshot = load();
    let case = snapshot["methods"]
        .as_array()
        .expect("快照缺 methods 数组")
        .iter()
        .find(|case| case["name"] == "frame.submit params")
        .expect("快照缺整面帧语料");
    let params: FrameSubmitParams =
        serde_json::from_value(case["value"].clone()).expect("快照整面帧应可反序列化");
    params.frame.validate().unwrap_or_else(|e| panic!("快照整面帧应通过校验：{e}"));

    // 语料兜底：指令、命中区、输入法锚点与帧率都出现过
    assert!(params.frame.commands.len() >= 3, "语料应覆盖多种绘制指令");
    assert!(!params.frame.hits.is_empty(), "语料应覆盖命中区");
    assert!(params.frame.ime.is_some(), "语料应覆盖输入法锚点");
    assert!(params.frame.fps.is_some(), "语料应覆盖帧率声明");

    // 命中按后声明优先：同一坐标落在两个命中区上时取后者
    let hits = &params.frame.hits;
    if hits.len() >= 2 {
        let (first, second) = (&hits[0], &hits[1]);
        let x = (first.x + first.w / 2.0).max(second.x + second.w / 2.0);
        let y = (first.y + first.h / 2.0).max(second.y + second.h / 2.0);
        if let Some(id) = params.frame.hit_at(x, y) {
            assert_eq!(id, second.id, "重叠处应取后声明的命中区");
        }
    }

    // 冻结编码：序列化产物与快照值逐字段一致
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        case["value"],
        "整面帧编码与快照不一致"
    );
}

#[test]
fn handshake_shape_is_frozen_and_version_checked() {
    let snapshot = load();
    let params: InitializeParams =
        serde_json::from_value(snapshot["handshake"]["params"].clone()).expect("握手 params 可反序列化");
    let result: InitializeResult =
        serde_json::from_value(snapshot["handshake"]["result"].clone()).expect("握手 result 可反序列化");

    assert_eq!(params.api_version, 1);
    assert_eq!(params.plugin.id, "com.atelyx.echo");
    assert_eq!(result.host.name, "Atelyx");
    assert!(handshake::compatible(params.api_version, result.api_version));
    assert!(!handshake::compatible(1, 2), "版本不一致必须判不兼容");
}

/// manifest 编码冻结：camelCase 字段名、平台入口映射、权限封闭枚举与未知字段忽略。
#[test]
fn manifest_shape_is_frozen() {
    let snapshot = load();
    let value = snapshot["manifest"].clone();
    let manifest: atelyx_plugin_protocol::manifest::Manifest =
        serde_json::from_value(value.clone()).expect("快照 manifest 应可反序列化");
    manifest.validate_shape().expect("快照 manifest 应通过结构校验");
    assert_eq!(manifest.entry.windows.as_deref(), Some("bin/studio.exe"));
    assert_eq!(
        manifest.permissions,
        vec![
            atelyx_plugin_protocol::manifest::Permission::Fs,
            atelyx_plugin_protocol::manifest::Permission::Process,
            atelyx_plugin_protocol::manifest::Permission::Notification,
        ]
    );
    assert!(manifest.singleton && !manifest.keep_mounted_on_vault_switch);
    assert_eq!(serde_json::to_value(&manifest).unwrap(), value, "manifest 编码与快照不一致");
}

/// host: 前缀判定：宿主保留命名空间的方向约定。
#[test]
fn host_prefix_is_reserved() {
    assert!(atelyx_plugin_protocol::is_host_method("host:view.event"));
    assert!(!atelyx_plugin_protocol::is_host_method("fs.readText"));
    // 非 host: 前缀的宿主方法名（如 hostile）不误判
    assert!(!atelyx_plugin_protocol::is_host_method("hostile.method"));
}

/// 请求 id 只认数字：字符串 id 与显式 null 都按形态非法拒绝。
#[test]
fn request_id_must_be_numeric() {
    let by_num = Envelope::parse(r#"{"jsonrpc":"2.0","id":3,"method":"m"}"#).unwrap();
    assert_eq!(by_num, Envelope::request(3, "m", Value::Null));
    for bad in [
        r#"{"jsonrpc":"2.0","id":"a-1","method":"m"}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#,
    ] {
        assert!(Envelope::parse(bad).is_err(), "应拒绝：{bad}");
    }
}

/// 能力面类型的 wire 形状逐域冻结：快照值反序列化再序列化必须与快照一致
/// （字段名漂移、Option 缺省语义变化都会在此现形）。
#[test]
fn method_type_shapes_are_frozen() {
    let snapshot = load();
    let cases = snapshot["methods"].as_array().expect("快照缺 methods 数组");
    assert!(cases.len() >= 15, "语料规模兜底：快照疑似被清空");

    for case in cases {
        let name = case["name"].as_str().unwrap_or_default();
        let value = case["value"].clone();
        let ok = match case["type"].as_str().unwrap_or_default() {
            "PathParams" => frozen_roundtrip::<PathParams>(&value),
            "TextContent" => frozen_roundtrip::<atelyx_plugin_protocol::method::TextContent>(&value),
            "BinaryContent" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::BinaryContent>(&value)
            }
            "WriteTextParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::WriteTextParams>(&value)
            }
            "WriteBinaryParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::WriteBinaryParams>(&value)
            }
            "ListResult" => frozen_roundtrip::<ListResult>(&value),
            "SpawnParams" => frozen_roundtrip::<atelyx_plugin_protocol::method::SpawnParams>(&value),
            "ProcessEventParams" => frozen_roundtrip::<ProcessEventParams>(&value),
            "ShowNotificationParams" => frozen_roundtrip::<ShowNotificationParams>(&value),
            "RegisterShortcutParams" => frozen_roundtrip::<RegisterShortcutParams>(&value),
            "TextPayload" => frozen_roundtrip::<TextPayload>(&value),
            "ClipboardImageResult" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::ClipboardImageResult>(&value)
            }
            "PickFileParams" => frozen_roundtrip::<PickFileParams>(&value),
            "PickedPath" => frozen_roundtrip::<PickedPath>(&value),
            "HttpRequestParams" => frozen_roundtrip::<HttpRequestParams>(&value),
            "HttpResult" => frozen_roundtrip::<HttpResult>(&value),
            "SurfaceAttachedParams" => frozen_roundtrip::<SurfaceAttachedParams>(&value),
            "RegisterParams" => frozen_roundtrip::<RegisterParams>(&value),
            "UnregisterParams" => frozen_roundtrip::<UnregisterParams>(&value),
            "DocOpenParams" => frozen_roundtrip::<DocOpenParams>(&value),
            "DocCloseParams" => frozen_roundtrip::<DocCloseParams>(&value),
            "TakeoverParams" => frozen_roundtrip::<TakeoverParams>(&value),
            "WindowOpenParams" => frozen_roundtrip::<WindowOpenParams>(&value),
            "WindowCloseParams" => frozen_roundtrip::<WindowCloseParams>(&value),
            "WindowClosedParams" => frozen_roundtrip::<WindowClosedParams>(&value),
            "VaultSwitchPayload" => frozen_roundtrip::<VaultSwitchPayload>(&value),
            "TableChangedPayload" => frozen_roundtrip::<TableChangedPayload>(&value),
            "ChatChangedPayload" => frozen_roundtrip::<ChatChangedPayload>(&value),
            "TableSnapshot" => frozen_roundtrip::<TableSnapshot>(&value),
            "TableUpdateCellParams" => frozen_roundtrip::<TableUpdateCellParams>(&value),
            "TableAddRowResult" => frozen_roundtrip::<TableAddRowResult>(&value),
            "TableRowIdParams" => frozen_roundtrip::<TableRowIdParams>(&value),
            "ImportFileParams" => frozen_roundtrip::<ImportFileParams>(&value),
            "FileChangedPayload" => frozen_roundtrip::<FileChangedPayload>(&value),
            "DeclareDefaultLayoutParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::DeclareDefaultLayoutParams>(&value)
            }
            "WindowUpdateParams" => frozen_roundtrip::<WindowUpdateParams>(&value),
            "WindowIdParams" => frozen_roundtrip::<WindowIdParams>(&value),
            "ShortcutQueryParams" => frozen_roundtrip::<ShortcutQueryParams>(&value),
            "ShortcutQueryResult" => frozen_roundtrip::<ShortcutQueryResult>(&value),
            "TableSetCellImageParams" => frozen_roundtrip::<TableSetCellImageParams>(&value),
            "ChatHistoryParams" => frozen_roundtrip::<ChatHistoryParams>(&value),
            "ChatRenameParams" => frozen_roundtrip::<ChatRenameParams>(&value),
            "ChatSessionParams" => frozen_roundtrip::<ChatSessionParams>(&value),
            "ChatSendParams" => frozen_roundtrip::<ChatSendParams>(&value),
            "RegisterToolParams" => frozen_roundtrip::<RegisterToolParams>(&value),
            "UnregisterToolParams" => frozen_roundtrip::<UnregisterToolParams>(&value),
            "ToolCallParams" => frozen_roundtrip::<ToolCallParams>(&value),
            "ToolResultParams" => frozen_roundtrip::<ToolResultParams>(&value),
            "ImageInfoResult" => frozen_roundtrip::<ImageInfoResult>(&value),
            "SetTrayItemsParams" => frozen_roundtrip::<SetTrayItemsParams>(&value),
            "TrayClickParams" => frozen_roundtrip::<TrayClickParams>(&value),
            "CollabPeersResult" => frozen_roundtrip::<CollabPeersResult>(&value),
            "CollabSendParams" => frozen_roundtrip::<CollabSendParams>(&value),
            "CollabSendResult" => frozen_roundtrip::<CollabSendResult>(&value),
            "CollabMessageParams" => frozen_roundtrip::<CollabMessageParams>(&value),
            "MeasureTextResult" => frozen_roundtrip::<MeasureTextResult>(&value),
            "ImageSource" => {
                frozen_roundtrip::<atelyx_plugin_protocol::paint::ImageSource>(&value)
            }
            "VaultRenameParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::VaultRenameParams>(&value)
            }
            "ClipboardWriteImageParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::ClipboardWriteImageParams>(&value)
            }
            "AiModelsResult" => frozen_roundtrip::<atelyx_plugin_protocol::method::AiModelsResult>(&value),
            "AiAgentsResult" => frozen_roundtrip::<atelyx_plugin_protocol::method::AiAgentsResult>(&value),
            "FrameSubmitParams" => frozen_roundtrip::<atelyx_plugin_protocol::frame::FrameSubmitParams>(&value),
            "FrameInputParams" => frozen_roundtrip::<atelyx_plugin_protocol::frame::FrameInputParams>(&value),
            "FrameTickParams" => frozen_roundtrip::<atelyx_plugin_protocol::frame::FrameTickParams>(&value),
            "MeasureBatchParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::MeasureBatchParams>(&value)
            }
            "MeasureBatchResult" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::MeasureBatchResult>(&value)
            }
            "ThemeResult" => frozen_roundtrip::<atelyx_plugin_protocol::method::ThemeResult>(&value),
            "ServiceRegisterParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::RegisterParams>(&value)
            }
            "ServiceListResult" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::ListResult>(&value)
            }
            "ServiceCallParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::CallParams>(&value)
            }
            "HostServiceCallParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::HostCallParams>(&value)
            }
            "ServiceResultParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::ResultParams>(&value)
            }
            "ServiceChangedParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::service::ChangedParams>(&value)
            }
            "MarkdownParams" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::MarkdownParams>(&value)
            }
            "MarkdownResult" => {
                frozen_roundtrip::<atelyx_plugin_protocol::method::MarkdownResult>(&value)
            }
            other => panic!("语料 {name}: 未知类型 {other}"),
        };
        assert!(ok, "语料 {name}: 编码与快照不一致");
    }
}

fn frozen_roundtrip<T: DeserializeOwned + Serialize>(value: &Value) -> bool {
    match serde_json::from_value::<T>(value.clone()) {
        Ok(parsed) => &serde_json::to_value(&parsed).unwrap() == value,
        Err(_) => false,
    }
}
