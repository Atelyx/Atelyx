//! 插件侧字号口径：宿主 `query.theme` 下发的字号档是唯一来源，本模块只做读取。

use std::sync::RwLock;

use atelyx_plugin_protocol::method::FontSizes;

/// 当前字号档（宿主在 `on_ready` 挂面之前下发；未下发时用缺省档，首帧不能按零字号排版）。
static FONT_SIZES: RwLock<Option<FontSizes>> = RwLock::new(None);

/// 主题未到时的缺省字号档（与宿主默认调色板同档）。
const DEFAULT_SIZES: FontSizes = FontSizes {
    micro: 11.0,
    caption: 12.0,
    ui: 13.0,
    body: 14.0,
    h2: 16.0,
    h1: 20.0,
};

/// 宿主主题字号档落地。
pub fn apply_font_sizes(sizes: FontSizes) {
    if let Ok(mut current) = FONT_SIZES.write() {
        *current = Some(sizes);
    }
}

/// 当前字号档：各档读取的唯一入口。
fn sizes() -> FontSizes {
    FONT_SIZES
        .read()
        .ok()
        .and_then(|sizes| sizes.clone())
        .unwrap_or(DEFAULT_SIZES)
}

/// 界面正文字号档（表格单元格默认字号）。
pub fn font_ui() -> f32 {
    sizes().ui
}

/// 说明文字号档（表头 / 状态栏 / 数字类单元格）。
pub fn font_caption() -> f32 {
    sizes().caption
}

/// 最小字号档（行号 / 省略提示）。
pub fn font_micro() -> f32 {
    sizes().micro
}

/// 正文档（编辑器浮层用）。
pub fn font_body() -> f32 {
    sizes().body
}
