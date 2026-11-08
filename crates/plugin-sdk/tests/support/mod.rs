//! 端到端测试的公共夹具：把 SDK 的 example 动态库当作插件产物安装给宿主。
//!
//! example 不产出 `CARGO_BIN_EXE_*`，路径按 cargo 的布局从测试可执行文件反推：
//! `<target>/<profile>/deps/<test>.exe` → `<target>/<profile>/examples/<name>.dll`。

use std::path::{Path, PathBuf};

use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

/// 插件入口在安装目录里的固定文件名（清单按平台声明它，测试不必关心原名）。
pub fn entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin.dll"
    } else {
        "libplugin.so"
    }
}

/// 某个 SDK example 的动态库产物路径。
pub fn example_library(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("取测试可执行文件路径失败");
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .expect("测试可执行文件不在预期目录结构里");
    let file = if cfg!(windows) {
        format!("{name}.dll")
    } else {
        format!("lib{name}.so")
    };
    let path = profile.join("examples").join(file);
    assert!(
        path.is_file(),
        "缺少 example 动态库：{}（先 cargo build -p atelyx-plugin-sdk --examples）",
        path.display()
    );
    path
}

/// 把一个 example 动态库铺成可安装的插件目录：写清单 + 拷入口。
/// `permissions` 是清单里的权限键，`extra` 追加在清单正文之后（如 `singleton = true`）。
pub fn stage(dir: &Path, id: &str, example: &str, permissions: &[&str], extra: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let declared = permissions
        .iter()
        .map(|name| format!("{name} = \"测试\"\n"))
        .collect::<String>();
    let table = if declared.is_empty() {
        String::new()
    } else {
        format!("\n[permissions]\n{declared}")
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"schema_version = "1.0.0"
id = "{id}"
name = "Fixture"
tagline = "测试夹具"
version = "1.0.0"
type = "add-on"
maintainer = "Test"
license = ["SPDX:Apache-2.0"]
host_version_min = "0.1.0"
api_version = 1
{extra}{table}
[entry]
windows = "{entry}"
linux = "{entry}"
"#,
            entry = entry_name()
        ),
    )
    .unwrap();
    std::fs::copy(example_library(example), dir.join(entry_name()))
        .expect("拷贝插件动态库失败");
}
