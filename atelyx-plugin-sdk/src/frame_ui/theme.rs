//! 控件默认样式：字号 / 圆角随宿主主题（`query.theme`），颜色一律走主题 token；
//! 控件高度与内边距是 SDK 常量，与宿主表单控件同档，保证观感一致。

use atelyx_plugin_protocol::paint::PaintColor;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::ColorToken;

/// 常规按钮高度（宿主 `btn_md`）。
pub const BUTTON_H: f32 = 32.0;
/// 表单输入与下拉触发高度（宿主 `dropdown_trigger`）。
pub const INPUT_H: f32 = 30.0;
/// 复选框方框边长（宿主 `checkbox`）。
pub const CHECK_BOX: f32 = 15.0;
/// 输入框与下拉的水平内边距。
pub const PAD_X: f32 = 8.0;
/// 按钮的水平内边距。
pub const BUTTON_PAD_X: f32 = 12.0;
/// 下拉展开后每个选项行的高度。
pub const OPTION_H: f32 = 26.0;
/// 下拉菜单与触发钮之间的空隙。
pub const OPTION_GAP: f32 = 2.0;

/// 宿主主题快照（取不到时用与宿主一致的缺省值）。
#[derive(Clone, Debug)]
pub struct Theme {
    pub sizes: FontSizes,
    pub radii: Radii,
    pub colors: ThemeColors,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
            radii: Radii { xs: 4.0, sm: 6.0, md: 10.0 },
            colors: ThemeColors::default(),
        }
    }
}

impl From<ThemeResult> for Theme {
    fn from(result: ThemeResult) -> Self {
        Self { sizes: result.font_sizes, radii: result.radii, colors: result.colors }
    }
}

impl Theme {
    /// 文本选区底色：宿主 `SELECTION` = 强调色 25% 不透明；accent 不可解析时退回强调软底。
    pub(crate) fn selection(&self) -> PaintColor {
        match parse_hex(&self.colors.accent) {
            Some([r, g, b, _]) => PaintColor::Rgba { rgba: [r, g, b, 64] },
            None => PaintColor::Token { name: ColorToken::AccentSoft },
        }
    }
}

/// `#rrggbb` / `#rrggbbaa` → RGBA。
fn parse_hex(value: &str) -> Option<[u8; 4]> {
    let hex = value.strip_prefix('#')?;
    let byte = |text: &str| u8::from_str_radix(text, 16).ok();
    match hex.len() {
        6 => Some([byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, 255]),
        8 => Some([byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?, byte(&hex[6..8])?]),
        _ => None,
    }
}

pub(crate) fn token(name: ColorToken) -> PaintColor {
    PaintColor::Token { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_uses_accent_with_a_quarter_alpha() {
        let theme = Theme {
            colors: ThemeColors { accent: "#e0a94e".into(), ..ThemeColors::default() },
            ..Theme::default()
        };
        assert_eq!(theme.selection(), PaintColor::Rgba { rgba: [0xe0, 0xa9, 0x4e, 64] });
    }

    #[test]
    fn unparsable_accent_falls_back_to_the_soft_token() {
        let theme = Theme::default();
        assert_eq!(theme.selection(), PaintColor::Token { name: ColorToken::AccentSoft });
    }
}
