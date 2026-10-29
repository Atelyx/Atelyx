//! 随包预置插件的同步：把发行包内插件目录补齐进用户插件根。
//! 只补装与升级，不覆盖用户的选择——停用保持停用，卸载过的不再装回（标记文件记录装过的 id）。

use std::collections::BTreeSet;
use std::path::Path;

use atelyx_plugin_protocol::manifest::parse_semver;
use serde::{Deserialize, Serialize};

use super::{install, layout, manifest};

/// 标记文件结构版本：不兼容的旧结构按错误处理，不能当空集——否则会把用户卸过的预置项装回。
const MARKER_SCHEMA: u32 = 1;

/// 一次同步的结果：装上的、更新的、装不上的（id 与可读原因）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetSyncReport {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub failed: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    schema: u32,
    /// 装过的预置插件 id：在此但目录已不存在 = 用户卸载过，不再自动装回。
    installed: BTreeSet<String>,
}

/// 逐条同步 `bundled` 里的插件目录：未装过的首装并置启用，已装的仅在随包版本更新时替换。
/// `marker` 读不出或结构不认识即整体放弃（宁可本次不装，也不能把用户卸过的装回）。
pub(crate) fn sync(
    root: &Path,
    bundled: &Path,
    marker_path: &Path,
) -> Result<PresetSyncReport, String> {
    let mut marked = read_marker(marker_path)?;
    let mut report = PresetSyncReport::default();
    let mut changed = false;
    for (dir_name, dir) in layout::discover(bundled) {
        let bundled_manifest = match manifest::read_manifest(&dir) {
            Ok(manifest) => manifest,
            Err(e) => {
                report.failed.push((dir_name, e));
                continue;
            }
        };
        if bundled_manifest.id != dir_name {
            report.failed.push((
                dir_name.clone(),
                format!("目录名与 manifest id 不一致：目录 {dir_name}，声明 {}", bundled_manifest.id),
            ));
            continue;
        }
        let id = bundled_manifest.id.clone();
        let installed_dir = layout::plugin_dir(root, id.as_str());
        if installed_dir.is_dir() {
            let installed_manifest = match manifest::read_manifest(&installed_dir) {
                Ok(manifest) => manifest,
                Err(e) => {
                    report.failed.push((id, e));
                    continue;
                }
            };
            if is_newer(&bundled_manifest.version, &installed_manifest.version) {
                match install::install_bundled(root, &dir) {
                    Ok(_) => report.updated.push(id.clone()),
                    Err(e) => {
                        report.failed.push((id, e));
                        continue;
                    }
                }
            }
        } else if marked.contains(&id) {
            // 装过又没了 = 用户卸载：尊重选择，不装回也不重复上报
            continue;
        } else {
            match install::install_bundled(root, &dir) {
                Ok(_) => {
                    // 首装即启用：启用失败整体回滚，下次启动仍按首装重试
                    if let Err(e) = layout::set_enabled(root, &id, true) {
                        let detail = match install::uninstall(root, &id) {
                            Ok(()) => e,
                            Err(rollback) => format!("{e}；回滚失败：{rollback}"),
                        };
                        report.failed.push((id, detail));
                        continue;
                    }
                    report.installed.push(id.clone());
                }
                Err(e) => {
                    report.failed.push((id, e));
                    continue;
                }
            }
        }
        // 装不上的不记：下次启动仍会重试
        changed |= marked.insert(id);
    }
    if changed {
        write_marker(marker_path, &marked)?;
    }
    Ok(report)
}

fn read_marker(path: &Path) -> Result<BTreeSet<String>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(format!("读取预置插件标记失败：{e}")),
    };
    let marker: Marker =
        serde_json::from_str(&raw).map_err(|e| format!("预置插件标记解析失败：{e}"))?;
    if marker.schema != MARKER_SCHEMA {
        return Err(format!("预置插件标记结构版本不支持：{}", marker.schema));
    }
    Ok(marker.installed)
}

fn write_marker(path: &Path, installed: &BTreeSet<String>) -> Result<(), String> {
    let marker = Marker { schema: MARKER_SCHEMA, installed: installed.clone() };
    let text = serde_json::to_string_pretty(&marker)
        .map_err(|e| format!("预置插件标记序列化失败：{e}"))?;
    crate::vault::atomic_write(path, &text)
        .map_err(|e| format!("写入预置插件标记失败：{e}"))
}

/// 随包条目是否比已装的新：版本串都可解析按语义版本比，任一不可解析则「不同即新」
/// （开发构建的临时版本号变化也应落到已装副本）。
fn is_newer(bundled: &str, installed: &str) -> bool {
    match (parse_semver(bundled), parse_semver(installed)) {
        (Some(bundled), Some(installed)) => bundled > installed,
        _ => bundled != installed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

    fn entry_name() -> &'static str {
        if cfg!(windows) {
            "app.exe"
        } else {
            "app"
        }
    }

    fn seed_plugin(dir: &Path, id: &str, version: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"{{"id":"{id}","name":"Demo","version":"{version}","apiVersion":1,"entry":{{"windows":"app.exe","linux":"app"}},"permissions":[]}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join(entry_name()), b"stub").unwrap();
    }

    /// 插件根、随包目录与标记文件三者都在临时目录下（随包目录不落在插件根内）。
    fn fixture(name: &str) -> (TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new(name);
        let root = tmp.join("installed");
        let bundled = tmp.join("bundled");
        let marker = tmp.join("preset-plugins.json");
        (tmp, root, bundled, marker)
    }

    #[test]
    fn first_run_installs_and_enables_but_records_no_source() {
        let (_tmp, root, bundled, marker) = fixture("preset-first");
        seed_plugin(&bundled.join("com.a.one"), "com.a.one", "1.0.0");
        let report = sync(&root, &bundled, &marker).expect("首装应成功");
        assert_eq!(report.installed, ["com.a.one"]);
        assert!(report.updated.is_empty());
        assert!(report.failed.is_empty());
        assert!(layout::plugin_dir(&root, "com.a.one").is_dir());
        assert!(layout::is_enabled(&root, "com.a.one"), "预置插件首装即启用");
        assert!(!layout::source_file(&root, "com.a.one").exists(), "预置内容不记安装来源");
        assert!(layout::is_preset(&root, "com.a.one"), "预置内容落预置标记");
    }

    #[test]
    fn second_run_is_a_no_op() {
        let (_tmp, root, bundled, marker) = fixture("preset-noop");
        seed_plugin(&bundled.join("com.a.one"), "com.a.one", "1.0.0");
        sync(&root, &bundled, &marker).unwrap();
        let again = sync(&root, &bundled, &marker).unwrap();
        assert!(again.installed.is_empty() && again.updated.is_empty(), "{again:?}");
    }

    #[test]
    fn a_user_uninstall_is_not_undone() {
        let (_tmp, root, bundled, marker) = fixture("preset-uninstall");
        seed_plugin(&bundled.join("com.a.one"), "com.a.one", "1.0.0");
        sync(&root, &bundled, &marker).unwrap();
        install::uninstall(&root, "com.a.one").unwrap();
        let again = sync(&root, &bundled, &marker).unwrap();
        assert!(again.installed.is_empty(), "卸载过的不再装回：{again:?}");
        assert!(!layout::plugin_dir(&root, "com.a.one").exists());
    }

    #[test]
    fn a_newer_bundle_updates_in_place_and_keeps_state() {
        let (_tmp, root, bundled, marker) = fixture("preset-update");
        let source = bundled.join("com.a.one");
        seed_plugin(&source, "com.a.one", "1.0.0");
        sync(&root, &bundled, &marker).unwrap();
        let data = layout::data_dir(&root, "com.a.one");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("state.json"), "{}").unwrap();
        layout::set_enabled(&root, "com.a.one", false).unwrap();

        seed_plugin(&source, "com.a.one", "1.1.0");
        let report = sync(&root, &bundled, &marker).unwrap();
        assert_eq!(report.updated, ["com.a.one"]);
        assert!(!layout::is_enabled(&root, "com.a.one"), "升级不改用户的停用选择");
        assert!(layout::is_preset(&root, "com.a.one"), "随包更新后仍是预置");
        assert!(data.join("state.json").is_file(), "私有数据随升级保留");
        let installed = manifest::read_manifest(&layout::plugin_dir(&root, "com.a.one")).unwrap();
        assert_eq!(installed.version, "1.1.0");
    }

    #[test]
    fn an_older_bundle_does_not_downgrade() {
        let (_tmp, root, bundled, marker) = fixture("preset-downgrade");
        let source = bundled.join("com.a.one");
        seed_plugin(&source, "com.a.one", "2.0.0");
        sync(&root, &bundled, &marker).unwrap();
        seed_plugin(&source, "com.a.one", "1.0.0");
        let report = sync(&root, &bundled, &marker).unwrap();
        assert!(report.updated.is_empty(), "{report:?}");
        let installed = manifest::read_manifest(&layout::plugin_dir(&root, "com.a.one")).unwrap();
        assert_eq!(installed.version, "2.0.0");
    }

    #[test]
    fn a_corrupt_marker_aborts_the_whole_sync() {
        let (_tmp, root, bundled, marker) = fixture("preset-corrupt");
        seed_plugin(&bundled.join("com.a.one"), "com.a.one", "1.0.0");
        std::fs::write(&marker, "not json").unwrap();
        assert!(sync(&root, &bundled, &marker).is_err());
        assert!(!layout::plugin_dir(&root, "com.a.one").exists(), "标记不可信时不装");
    }

    #[test]
    fn a_dir_name_mismatch_is_reported_not_installed() {
        let (_tmp, root, bundled, marker) = fixture("preset-mismatch");
        seed_plugin(&bundled.join("com.a.wrong"), "com.a.one", "1.0.0");
        let report = sync(&root, &bundled, &marker).unwrap();
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "com.a.wrong");
        assert!(!layout::plugin_dir(&root, "com.a.one").exists());
    }
}
