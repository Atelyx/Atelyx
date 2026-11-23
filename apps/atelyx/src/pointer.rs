//! 系统指针位置（全局判定用）：系统文件拖入时 iced 只给路径不给落点，需读系统光标。

/// 屏幕坐标的指针位置（物理像素）。Windows 直接查系统光标；其他平台无标准查询，返回 None。
pub fn screen_cursor() -> Option<iced::Point> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return None;
        }
        Some(iced::Point::new(point.x as f32, point.y as f32))
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 屏幕物理坐标 → 窗口局部逻辑坐标（`window_pos` 为窗口外框的逻辑屏幕坐标）。
pub fn window_local(
    screen: iced::Point,
    window_pos: iced::Point,
    scale: f32,
) -> Option<iced::Point> {
    if scale <= 0.0 {
        return None;
    }
    Some(iced::Point::new(
        screen.x / scale - window_pos.x,
        screen.y / scale - window_pos.y,
    ))
}

#[cfg(test)]
mod tests {
    use super::window_local;
    use iced::Point;

    #[test]
    fn window_local_converts_physical_screen_to_logical_offset() {
        // 150% 缩放、窗口外框在 (100, 50)，光标物理 (400, 350) → 窗口内 (166.67, 183.33)
        let local = window_local(Point::new(400.0, 350.0), Point::new(100.0, 50.0), 1.5)
            .expect("缩放有效");
        assert!((local.x - 166.666_66).abs() < 1e-3);
        assert!((local.y - 183.333_33).abs() < 1e-3);
    }

    #[test]
    fn window_local_rejects_invalid_scale() {
        assert!(window_local(Point::new(400.0, 350.0), Point::new(0.0, 0.0), 0.0).is_none());
    }
}
