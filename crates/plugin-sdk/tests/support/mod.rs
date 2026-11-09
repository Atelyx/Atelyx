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

/// 端到端测试用的宿主动面：绘制期的量算 / Markdown / 主题都按固定口径回答。
/// 只有面帧相关的 e2e 会用到它，其余 e2e 共用本模块时允许闲置。
#[allow(dead_code)]
pub struct TestHost;

impl atelyx_core::plugin_runtime::DrawHost for TestHost {
    fn measure(
        &self,
        request: atelyx_core::plugin_runtime::MeasureRequest<'_>,
    ) -> atelyx_plugin_protocol::method::MeasureTextResult {
        let width: f32 = request
            .text
            .chars()
            .map(|c| if c.is_ascii() { 0.5 } else { 1.0 })
            .sum::<f32>()
            * request.size;
        let lines = match request.max_width {
            Some(limit) if width > limit => (width / limit).ceil().max(1.0) as u32,
            _ => 1,
        };
        atelyx_plugin_protocol::method::MeasureTextResult {
            width: width.min(request.max_width.unwrap_or(width)),
            height: request.size * 1.2 * lines as f32,
            lines,
        }
    }

    fn markdown(
        &self,
        content: &str,
    ) -> Result<Vec<atelyx_plugin_protocol::method::MarkdownBlock>, String> {
        Ok(vec![atelyx_plugin_protocol::method::MarkdownBlock::Paragraph {
            depth: 0,
            spans: vec![atelyx_plugin_protocol::method::MarkdownSpan {
                text: content.to_string(),
                strong: false,
                code: false,
                link: None,
            }],
        }])
    }

    fn theme(&self) -> atelyx_plugin_protocol::method::ThemeResult {
        atelyx_plugin_protocol::method::ThemeResult {
            colors: atelyx_plugin_protocol::method::ThemeColors {
                accent: "#e0a94e".into(),
                ..Default::default()
            },
            font_sizes: atelyx_plugin_protocol::method::FontSizes {
                micro: 11.0,
                caption: 12.0,
                ui: 13.0,
                body: 14.0,
                h2: 16.0,
                h1: 20.0,
            },
            radii: atelyx_plugin_protocol::method::Radii { xs: 4.0, sm: 6.0, md: 10.0 },
            event_colors: Vec::new(),
        }
    }
}

/// 模拟宿主重绘拍里的那一次同步绘制。
#[allow(dead_code)]
pub fn draw(
    runtime: &atelyx_core::plugin_runtime::PluginRuntime,
    id: &str,
    surface: &str,
    width: f32,
    height: f32,
) -> Option<std::sync::Arc<atelyx_plugin_protocol::frame::SurfaceFrame>> {
    match runtime.draw_surface(id, surface, width, height, &TestHost) {
        atelyx_core::plugin_runtime::DrawOutcome::Painted(frame) => Some(frame),
        atelyx_core::plugin_runtime::DrawOutcome::Skipped => None,
    }
}
