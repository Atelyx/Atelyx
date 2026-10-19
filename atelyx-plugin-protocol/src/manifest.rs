//! 插件 manifest（`plugin.json`）：插件自述的身份、入口、能力声明与贡献，宿主装载前据此校验。
//! 本模块只含纯结构与序列化；平台入口存在性、协议版本一致性等需要文件系统的校验归宿主运行时。

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
    /// 协议版本，必须等于宿主的 [`crate::API_VERSION`]。
    pub api_version: u32,
    pub entry: Entry,
    /// 能力声明清单：未声明的能力调用一律被宿主拒绝（fail-closed）。
    pub permissions: Vec<Permission>,
    /// 进程型插件的单实例约束（多窗口共享同一宿主运行时全局单例）。
    pub singleton: bool,
    /// 切换仓库容器（本地 ↔ 空间 / 换仓库）时保持挂载；false = 宿主重载插件进程。
    pub keep_mounted_on_vault_switch: bool,
    pub doc_type_registrations: Vec<DocTypeRegistration>,
}

/// 按平台的可执行入口（相对插件目录的路径）。缺失 = 该平台不支持。
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

/// 领域文档类型注册：扩展名、展示标签、图标名与承载文档内容的贡献 id。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocTypeRegistration {
    /// 不带点的扩展名（如 `board`）。
    pub extension: String,
    pub label: String,
    /// 宿主图标形状表中的图标名。
    pub icon: String,
    /// 承载该文档实例界面树的贡献 id（插件须同时以 `view.register` 注册同名贡献，
    /// `slot` 为面板）；打开该扩展名文件时宿主按文件实例分配 surfaceId 并通知插件提交树。
    pub surface: String,
}

/// 纯结构校验：id 形态与目录安全、名称/版本非空、协议版本已填。平台入口存在性与
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
        for reg in &self.doc_type_registrations {
            if reg.extension.trim().is_empty() || reg.extension.contains('.') {
                return Err(format!("文档类型扩展名非法：{}", reg.extension));
            }
            if reg.surface.trim().is_empty() {
                return Err(format!("文档类型 {} 缺承载贡献 id（surface）", reg.extension));
            }
        }
        Ok(())
    }
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
}
