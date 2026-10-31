//! 已安装 / 待安装插件的 manifest 读取与校验：结构校验之外还需文件系统事实
//! （协议版本一致、当前平台入口存在且不越出插件目录）。

use std::path::{Component, Path};

use atelyx_plugin_protocol::manifest::{parse_semver, Manifest, MANIFEST_FILE};
use atelyx_plugin_protocol::API_VERSION;

/// 校验插件声明的 `minHostVersion` 与宿主产品版本的关系（不满足即拒收，不静默放行）。
/// 与 `read_manifest` 分开：安装落位不拦版本，装载 / 启用时才按宿主实际版本判定。
pub(crate) fn check_host_version(manifest: &Manifest, host_version: &str) -> Result<(), String> {
    let Some(min) = &manifest.min_host_version else {
        return Ok(());
    };
    if host_at_least(host_version, min) {
        Ok(())
    } else {
        Err(format!("宿主版本过低：插件要求最低 {min}，当前 {host_version}"))
    }
}

/// 宿主版本是否达到插件声明的最低要求（任一形态非法都按不满足处理）。
fn host_at_least(host_version: &str, min: &str) -> bool {
    let Some(host) = parse_semver(host_version) else { return false };
    let Some(min) = parse_semver(min) else { return false };
    host >= min
}

/// 从插件目录读取并校验 manifest。任一校验不过即返回可读原因（拒绝装载，不静默降级）。
pub(crate) fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("读取 {MANIFEST_FILE} 失败：{e}"))?;
    let manifest: Manifest =
        serde_json::from_str(&raw).map_err(|e| format!("{MANIFEST_FILE} 解析失败：{e}"))?;
    manifest.validate_shape()?;
    if manifest.api_version != API_VERSION {
        return Err(format!(
            "协议版本不匹配：插件 apiVersion {}，宿主 {API_VERSION}",
            manifest.api_version
        ));
    }
    if let Some(abi) = manifest.abi_version {
        if abi != atelyx_plugin_abi::ABI_VERSION {
            return Err(format!(
                "ABI 版本不匹配：插件 abiVersion {abi}，宿主 {}",
                atelyx_plugin_abi::ABI_VERSION
            ));
        }
    }
    let entry = manifest
        .entry
        .for_current_platform()
        .ok_or_else(|| "未声明当前平台的可执行入口".to_string())?;
    validate_entry(entry)?;
    if !dir.join(entry).is_file() {
        return Err(format!("入口文件不存在：{entry}"));
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

    fn manifest_with_min(min: Option<&str>) -> Manifest {
        Manifest {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: "1.0.0".into(),
            api_version: API_VERSION,
            min_host_version: min.map(str::to_string),
            entry: Entry { windows: Some("app.exe".into()), linux: Some("app".into()) },
            ..Manifest::default()
        }
    }

    #[test]
    fn min_host_version_gate_rejects_too_new_plugins() {
        let host = "0.6.0";
        assert!(check_host_version(&manifest_with_min(None), host).is_ok(), "未声明应放行");
        assert!(check_host_version(&manifest_with_min(Some("0.4.0")), host).is_ok(), "低于宿主应放行");
        assert!(check_host_version(&manifest_with_min(Some("0.6")), host).is_ok(), "缺段视为等值");
        let err = check_host_version(&manifest_with_min(Some("99.0.0")), host).expect_err("高于宿主应拒");
        assert!(err.contains("宿主版本过低"), "{err}");
        assert!(
            check_host_version(&manifest_with_min(Some("not-a-version")), host).is_err(),
            "非法版本串应拒"
        );
    }
}
