//! 已安装 / 待安装插件的清单读取与校验：结构校验之外还需文件系统事实
//! （协议版本一致、当前平台入口存在且不越出插件目录）。

use std::path::{Component, Path};

use atelyx_plugin_protocol::manifest::{
    current_platform, parse_semver, Manifest, MANIFEST_FILE, SCHEMA_VERSION,
};
use atelyx_plugin_protocol::API_VERSION;

/// 校验清单声明的宿主版本区间与当前宿主产品版本的关系（不满足即拒收，不静默放行）。
/// 与 `read_manifest` 分开：安装落位不拦版本，装载 / 启用时才按宿主实际版本判定。
pub(crate) fn check_host_version(manifest: &Manifest, host_version: &str) -> Result<(), String> {
    let Some(host) = parse_semver(host_version) else {
        return Err(format!("宿主版本号非法：{host_version}"));
    };
    let Some(min) = parse_semver(&manifest.host_version_min) else {
        return Err(format!("host_version_min 非法：{}", manifest.host_version_min));
    };
    if host < min {
        return Err(format!(
            "宿主版本过低：插件要求最低 {}，当前 {host_version}",
            manifest.host_version_min
        ));
    }
    if let Some(max) = &manifest.host_version_max {
        let Some(max) = parse_semver(max) else {
            return Err(format!("host_version_max 非法：{max}"));
        };
        if host > max {
            return Err(format!(
                "宿主版本过高：插件支持到 {}，当前 {host_version}",
                manifest.host_version_max.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(())
}

/// 从插件目录读取并校验清单。任一校验不过即返回可读原因（拒绝装载，不静默降级）。
pub(crate) fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {MANIFEST_FILE} 失败：{e}"))?;
    let manifest = parse(&raw)?;
    manifest.validate_shape()?;
    if manifest.api_version != API_VERSION {
        return Err(format!(
            "协议版本不匹配：插件 api_version {}，宿主 {API_VERSION}",
            manifest.api_version
        ));
    }
    if !manifest.supports_current_platform() {
        return Err(format!("插件未声明当前平台（{}）", current_platform()));
    }
    let entry = manifest
        .entry
        .for_current_platform()
        .ok_or_else(|| "未声明当前平台的插件入口".to_string())?;
    validate_entry(entry)?;
    if !dir.join(entry).is_file() {
        return Err(format!("入口文件不存在：{entry}"));
    }
    Ok(manifest)
}

/// 解析清单文本；格式版本不识别时给出比 TOML 定位信息更直接的一句原因。
fn parse(raw: &str) -> Result<Manifest, String> {
    let manifest: Manifest =
        toml::from_str(raw).map_err(|e| format!("{MANIFEST_FILE} 解析失败：{e}"))?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "{MANIFEST_FILE} 的 schema_version 不识别：{}（要求 {SCHEMA_VERSION}）",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

/// 入口必须是插件目录内的相对路径：绝对路径、`..`、根与盘符前缀一律拒绝（防越出目录）。
fn validate_entry(rel: &str) -> Result<(), String> {
    if rel.trim().is_empty() {
        return Err("入口路径为空".to_string());
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(format!("入口须为相对路径：{rel}"));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!("入口路径非法（不得含 .. 或根前缀）：{rel}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::manifest::Entry;

    fn manifest_with(min: &str, max: Option<&str>) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION.into(),
            id: "com.example.demo".into(),
            name: "Demo".into(),
            tagline: "示例".into(),
            version: "1.0.0".into(),
            kind: "add-on".into(),
            tags: Vec::new(),
            maintainer: "Example".into(),
            license: vec!["SPDX:Apache-2.0".into()],
            copyright: Vec::new(),
            website: None,
            platforms: Vec::new(),
            host_version_min: min.into(),
            host_version_max: max.map(str::to_string),
            api_version: API_VERSION,
            singleton: false,
            keep_mounted_on_vault_switch: false,
            permissions: Default::default(),
            entry: Entry {
                windows: Some("app.dll".into()),
                linux: Some("app.so".into()),
            },
            build: Default::default(),
            doc_type_registrations: Vec::new(),
        }
    }

    #[test]
    fn host_version_range_gates_both_ends() {
        let host = "0.6.0";
        assert!(check_host_version(&manifest_with("0.4.0", None), host).is_ok(), "低于宿主应放行");
        assert!(check_host_version(&manifest_with("0.6", None), host).is_ok(), "缺段视为等值");
        assert!(check_host_version(&manifest_with("0.4.0", Some("0.9.0")), host).is_ok());
        let err = check_host_version(&manifest_with("99.0.0", None), host).expect_err("高于宿主应拒");
        assert!(err.contains("宿主版本过低"), "{err}");
        let err =
            check_host_version(&manifest_with("0.1.0", Some("0.5.0")), host).expect_err("超上限应拒");
        assert!(err.contains("宿主版本过高"), "{err}");
        assert!(check_host_version(&manifest_with("not-a-version", None), host).is_err(), "非法下限应拒");
        assert!(check_host_version(&manifest_with("0.1.0", Some("oops")), host).is_err(), "非法上限应拒");
    }

    /// 旧格式清单（JSON）不解析成功即为拒收，不做双读识别。
    #[test]
    fn legacy_json_manifest_is_rejected() {
        let err = parse(r#"{"id":"com.example.demo"}"#).expect_err("JSON 不是合法 TOML 清单");
        assert!(err.contains(MANIFEST_FILE), "{err}");
    }

    #[test]
    fn unknown_schema_version_is_rejected_with_reason() {
        let raw = toml::to_string(&manifest_with("0.1.0", None)).unwrap();
        assert!(parse(&raw).is_ok());
        let bumped = raw.replace("schema_version = \"1.0.0\"", "schema_version = \"2.0.0\"");
        let err = parse(&bumped).expect_err("未识别版本应拒");
        assert!(err.contains("schema_version"), "{err}");
    }
}
