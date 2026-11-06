//! 插件 manifest（`plugin.json`）：插件自述的身份、入口、能力声明与贡献，宿主装载前据此校验。
//! 本模块只含纯结构与序列化；平台入口存在性、契约版本一致性等需要文件系统的校验归宿主运行时。

use serde::{Deserialize, Serialize};

/// manifest 文件名（固定放在插件目录根部）。
pub const MANIFEST_FILE: &str = "plugin.json";

/// 插件 manifest。缺字段按 [`Default`] 补齐，未知字段忽略。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Manifest {
    /// 反向域名 id（如 `com.atelyx.echo`），发布后不可变；同时是插件目录名与隔离命名空间。
    pub id: String,
    pub name: String,
    pub version: String,
    /// 契约版本（必填），必须等于宿主的 [`crate::API_VERSION`]；缺失按 0 处理并被结构校验拒绝。
    pub api_version: u32,
    /// 最低宿主版本（语义化版本，如 `0.4.0`）；缺省 = 不限。宿主低于它即拒绝装载。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_host_version: Option<String>,
    pub entry: Entry,
    /// 能力声明清单：用于向用户公示与审计，不拦截调用——插件与宿主同进程、持同等权限，
    /// 闸门拦住的是守规矩的插件。宿主据此标出「用到但未声明」的调用。
    pub permissions: Vec<Permission>,
    /// 单实例约束（多窗口共享同一宿主运行时全局单例）。
    pub singleton: bool,
    /// 切换仓库容器（本地 ↔ 空间 / 换仓库）时保持挂载；false = 宿主停掉实例后按新容器重启。
    pub keep_mounted_on_vault_switch: bool,
    pub doc_type_registrations: Vec<DocTypeRegistration>,
}

/// 按平台的入口（可执行文件或动态库，相对插件目录的路径）。缺失 = 该平台不支持。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<String>,
}

impl Entry {
    /// 当前平台的可执行相对路径；缺失即本平台不支持装载。
    pub fn for_current_platform(&self) -> Option<&str> {
        #[cfg(target_os = "windows")]
        {
            self.windows.as_deref()
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.linux.as_deref()
        }
    }
}

/// 能力面枚举（封闭）：扩容随协议升版。名字与宿主能力面一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    Fs,
    Http,
    Process,
    State,
    Notification,
    Shortcuts,
    Vault,
    Clipboard,
    Window,
    Tray,
    Collab,
    Ai,
    Dialog,
}

/// 领域文档类型注册：声明扩展名、内容真源的归属插件与可承载它的视图贡献。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DocTypeRegistration {
    /// 插件内稳定的文档类型 id（如 `table`）；宿主状态与路由以它为准。
    pub id: String,
    /// 不带点的扩展名（如 `board`）；一个类型可声明多个，比较不区分大小写。
    pub extensions: Vec<String>,
    /// 持内容真源、定解析 / 合并 / 保存语义的插件 id；通常与声明插件相同。
    pub owner_plugin: String,
    /// 能承载该文档类型的视图贡献 id 列表（贡献由声明插件注册）。
    pub views: Vec<String>,
    /// 文档内容可编辑；false = 只读。
    pub editable: bool,
    /// 允许同类多文档并行打开（false = 每类型至多一个热文档）。
    pub multi_doc: bool,
    pub label: String,
    /// 宿主内置形状名（未知名回落拼图）。
    pub icon: String,
}

/// 纯结构校验：id 形态与目录安全、名称/版本非空、契约版本已填。平台入口存在性与
/// 版本一致性依赖文件系统与宿主上下文，由宿主运行时校验。
impl Manifest {
    pub fn validate_shape(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err("name 不能为空".into());
        }
        if self.version.trim().is_empty() {
            return Err("version 不能为空".into());
        }
        if self.api_version == 0 {
            return Err("apiVersion 不能为空或 0".into());
        }
        if let Some(min) = &self.min_host_version {
            if parse_semver(min).is_none() {
                return Err(format!("minHostVersion 非法（须为点分数字版本，如 0.4 或 0.4.0）：{min}"));
            }
        }
        let mut seen_ids: Vec<&str> = Vec::new();
        let mut seen_extensions: Vec<&str> = Vec::new();
        for reg in &self.doc_type_registrations {
            if reg.id.trim().is_empty() {
                return Err("文档类型 id 不能为空".into());
            }
            if seen_ids.contains(&reg.id.trim()) {
                return Err(format!("文档类型 id 重复：{}", reg.id));
            }
            seen_ids.push(reg.id.trim());
            if reg.extensions.is_empty() {
                return Err(format!("文档类型 {} 未声明扩展名", reg.id));
            }
            for extension in &reg.extensions {
                let extension = extension.trim();
                if extension.is_empty() || extension.contains('.') {
                    return Err(format!("文档类型 {} 扩展名非法：{extension}", reg.id));
                }
                if seen_extensions.iter().any(|seen| seen.eq_ignore_ascii_case(extension)) {
                    return Err(format!("扩展名在本 manifest 内重复声明：{extension}"));
                }
                seen_extensions.push(extension);
            }
            if reg.owner_plugin.trim().is_empty() {
                return Err(format!("文档类型 {} 缺归属插件（ownerPlugin）", reg.id));
            }
            validate_id(reg.owner_plugin.trim())
                .map_err(|_| format!("文档类型 {} 的 ownerPlugin 非法：{}", reg.id, reg.owner_plugin))?;
            if reg.views.is_empty() {
                return Err(format!("文档类型 {} 未声明承载视图（views）", reg.id));
            }
            for view in &reg.views {
                if view.trim().is_empty() {
                    return Err(format!("文档类型 {} 的视图贡献 id 不能为空", reg.id));
                }
            }
            if reg.label.trim().is_empty() {
                return Err(format!("文档类型 {} 缺标签（label）", reg.id));
            }
        }
        Ok(())
    }
}

/// 宽松语义化版本解析：`x` / `x.y` / `x.y.z`（缺段补 0），可带 `-` 预发布后缀（忽略）。
/// 返回 `None` = 非法形态。宿主与插件两侧共用同一口径做版本比较。
pub fn parse_semver(version: &str) -> Option<(u32, u32, u32)> {
    let core = version.trim().split('-').next().unwrap_or("");
    if core.is_empty() {
        return None;
    }
    let mut parts = [0u32; 3];
    let mut count = 0;
    for (index, segment) in core.split('.').enumerate() {
        if index >= 3 {
            return None;
        }
        if segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        parts[index] = segment.parse().ok()?;
        count += 1;
    }
    (count >= 1).then_some((parts[0], parts[1], parts[2]))
}

/// 反向域名 id：至少两段、每段为字母数字或 `-` 且不以 `-` 起止；同时用作目录名，
/// 故禁止路径分隔符与点开头（避免与宿主保留的隐藏目录混淆）。
fn validate_id(id: &str) -> Result<(), String> {
    if id.starts_with('.') {
        return Err(format!("id 不得以点开头：{id}"));
    }
    if id.contains(['/', '\\']) {
        return Err(format!("id 不得含路径分隔符：{id}"));
    }
    let labels: Vec<&str> = id.split('.').collect();
    if labels.len() < 2 {
        return Err(format!("id 须为反向域名（至少两段）：{id}"));
    }
    for label in labels {
        let valid = !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Err(format!("id 段非法：{label}（{id}）"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Manifest {
        Manifest {
            id: "com.atelyx.echo".into(),
            name: "Echo".into(),
            version: "0.1.0".into(),
            api_version: 1,
            entry: Entry { windows: Some("echo.exe".into()), linux: Some("echo".into()) },
            permissions: vec![Permission::State],
            ..Manifest::default()
        }
    }

    #[test]
    fn shape_accepts_valid_manifest() {
        valid().validate_shape().expect("合法 manifest 应通过");
    }

    #[test]
    fn shape_rejects_bad_ids() {
        for bad in ["echo", "com.", ".com.x", "com..x", "com.at el yx", "com.a/b", "com.-a", "com.a-"] {
            let mut m = valid();
            m.id = bad.into();
            assert!(m.validate_shape().is_err(), "应拒绝 id：{bad}");
        }
    }

    #[test]
    fn shape_rejects_empty_fields_and_zero_api_version() {
        for mutate in [
            (|m: &mut Manifest| m.name = " ".into()) as fn(&mut Manifest),
            |m: &mut Manifest| m.version = "".into(),
            |m: &mut Manifest| m.api_version = 0,
        ] {
            let mut m = valid();
            mutate(&mut m);
            assert!(m.validate_shape().is_err());
        }
    }

    #[test]
    fn serde_uses_camel_case_and_defaults_missing() {
        let m: Manifest = serde_json::from_str(
            r#"{"id":"com.atelyx.echo","name":"Echo","version":"0.1.0","apiVersion":1,
                "entry":{"windows":"echo.exe"},"permissions":["state","notification"]}"#,
        )
        .unwrap();
        assert_eq!(m.entry.linux, None);
        assert_eq!(m.permissions, vec![Permission::State, Permission::Notification]);
        assert!(!m.singleton);
        assert!(!m.keep_mounted_on_vault_switch);
        // 未知字段忽略、缺省字段不写回
        let out = serde_json::to_string(&m).unwrap();
        assert!(!out.contains("\"linux\""));
        assert!(out.contains("\"apiVersion\":1"));
    }

    #[test]
    fn missing_api_version_is_invalid() {
        let m: Manifest = serde_json::from_str(
            r#"{"id":"com.atelyx.echo","name":"Echo","version":"0.1.0",
                "entry":{"windows":"echo.exe"}}"#,
        )
        .unwrap();
        assert_eq!(m.api_version, 0, "缺 apiVersion 按 0 解析");
        assert!(m.validate_shape().is_err(), "缺 apiVersion 必须判为非法 manifest");
    }

    fn doc_type(id: &str, extension: &str) -> DocTypeRegistration {
        DocTypeRegistration {
            id: id.into(),
            extensions: vec![extension.into()],
            owner_plugin: "com.atelyx.echo".into(),
            views: vec![format!("{id}-view")],
            editable: true,
            multi_doc: false,
            label: id.into(),
            icon: String::new(),
        }
    }

    #[test]
    fn shape_accepts_doc_type_registrations() {
        let mut m = valid();
        m.doc_type_registrations = vec![doc_type("note", "md"), doc_type("board", "board")];
        m.validate_shape().expect("合法文档类型注册应通过");
    }

    #[test]
    fn shape_rejects_bad_doc_type_registrations() {
        let mut m = valid();
        m.doc_type_registrations = vec![DocTypeRegistration::default()];
        assert!(m.validate_shape().is_err(), "缺字段的注册必须拒绝");

        let mut duplicated_id = doc_type("note", "md");
        duplicated_id.id = "note".into();
        let mut m = valid();
        m.doc_type_registrations = vec![duplicated_id.clone(), duplicated_id];
        assert!(m.validate_shape().is_err(), "同一 manifest 内 id 重复必须拒绝");

        let mut m = valid();
        m.doc_type_registrations = vec![doc_type("note", "md"), doc_type("other", "MD")];
        assert!(m.validate_shape().is_err(), "同一 manifest 内扩展名重复（不分大小写）必须拒绝");

        let mut bad_owner = doc_type("note", "md");
        bad_owner.owner_plugin = "not-a-plugin-id".into();
        let mut m = valid();
        m.doc_type_registrations = vec![bad_owner];
        assert!(m.validate_shape().is_err(), "ownerPlugin 必须是合法插件 id");
    }

    #[test]
    fn abi_version_field_is_ignored() {
        // 只留旧字段不构成契约版本声明：必需字段仍缺
        let m: Manifest = serde_json::from_str(
            r#"{"id":"com.atelyx.echo","name":"Echo","version":"0.1.0","abiVersion":1,
                "entry":{"windows":"echo.exe"}}"#,
        )
        .unwrap();
        assert!(m.validate_shape().is_err(), "abiVersion 不得替代 apiVersion");
        // 同时声明时旧字段按未知字段忽略，不回写
        let m: Manifest = serde_json::from_str(
            r#"{"id":"com.atelyx.echo","name":"Echo","version":"0.1.0","apiVersion":1,"abiVersion":1,
                "entry":{"windows":"echo.exe"}}"#,
        )
        .unwrap();
        m.validate_shape().expect("结构合法");
        assert!(!serde_json::to_string(&m).unwrap().contains("\"abiVersion\""));
    }
}
