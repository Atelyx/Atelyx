//! 测试夹具：清单与插件目录的最小可装载形态（只在测试构建里编译）。

use std::path::Path;

use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

/// 最小可装载清单的 TOML 文本。`extra` 追加在正文之后（如权限表与文档类型段）。
pub(crate) fn manifest_toml(id: &str, extra: &str) -> String {
    format!(
        r#"schema_version = "1.0.0"
id = "{id}"
name = "Demo"
tagline = "测试插件"
version = "1.0.0"
type = "add-on"
maintainer = "Test"
license = ["SPDX:Apache-2.0"]
host_version_min = "0.1.0"
api_version = 1
{extra}
[entry]
windows = "app.exe"
linux = "app"
"#
    )
}

/// 当前平台的入口占位文件名。
pub(crate) fn entry_name() -> &'static str {
    if cfg!(windows) {
        "app.exe"
    } else {
        "app"
    }
}

/// 造一个可扫描 / 可安装的插件目录（合法清单 + 平台入口占位文件）。
pub(crate) fn seed_plugin(dir: &Path, id: &str, extra: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_toml(id, extra)).unwrap();
    std::fs::write(dir.join(entry_name()), b"stub").unwrap();
}
