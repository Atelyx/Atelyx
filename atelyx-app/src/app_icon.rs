//! 应用图标资源与句柄：窗口图标与界面上展示的同一枚 PNG。

/// 应用图标（随应用分发的 PNG 资源）。
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icons/icon.png");

/// 应用图标句柄（进程内单例：同一 Handle 复用纹理，避免每帧重上传）。
pub fn icon_handle() -> iced::widget::image::Handle {
    static HANDLE: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| iced::widget::image::Handle::from_bytes(ICON_PNG.to_vec()))
        .clone()
}

/// 窗口图标（任务栏 / Alt-Tab / 窗口切换器）；构造失败回退系统默认图标。
pub fn window_icon() -> Option<iced::window::icon::Icon> {
    iced::window::icon::from_file_data(ICON_PNG, None).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌图标资源损坏（重新编码出错 / 文件被换）须在测试里暴露，而不是等建窗时才表现成无图标。
    #[test]
    fn embedded_icon_png_decodes_into_window_icon() {
        assert!(window_icon().is_some());
    }
}
