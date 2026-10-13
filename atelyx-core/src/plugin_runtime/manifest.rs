//! 已安装 / 待安装插件的 manifest 读取与校验：结构校验之外还需文件系统事实
//! （协议版本一致、当前平台入口存在且不越出插件目录）。

use std::path::{Component, Path};

use atelyx_plugin_protocol::manifest::{Manifest, MANIFEST_FILE};
use atelyx_plugin_protocol::API_VERSION;

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
