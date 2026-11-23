//! 主窗口显隐：关闭按钮只把窗口藏起来，托盘唤回时经系统重新显示并前台化。
//! Windows 直接改原生窗口可见态（iced 没有可见性命令）；其余平台按最小化 / 还原对齐。

use iced::{window, Task};

use crate::Message;

/// 隐藏主窗口（退出任务栏，进程留在托盘里）。
pub fn hide(id: window::Id) -> Task<Message> {
    platform::set_visible(id, false).map(|()| Message::Noop)
}

/// 显示主窗口并取得焦点（最小化状态先还原）。
pub fn show(id: window::Id) -> Task<Message> {
    platform::set_visible(id, true)
        .chain(window::gain_focus(id))
        .map(|()| Message::Noop)
}

#[cfg(windows)]
mod platform {
    use iced::{window, Task};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW,
    };

    /// 经 winit 给出的原生句柄改可见态；取不到句柄就保持原状。
    pub(super) fn set_visible(id: window::Id, visible: bool) -> Task<()> {
        window::run(id, move |window| {
            use iced::window::raw_window_handle::RawWindowHandle;
            let Ok(handle) = window.window_handle() else { return };
            let RawWindowHandle::Win32(raw) = handle.as_raw() else { return };
            let hwnd = HWND(raw.hwnd.get() as *mut core::ffi::c_void);
            unsafe {
                if visible {
                    let command = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
                    let _ = ShowWindow(hwnd, command);
                } else {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        })
    }
}

#[cfg(not(windows))]
mod platform {
    use iced::{window, Task};

    pub(super) fn set_visible(id: window::Id, visible: bool) -> Task<()> {
        window::minimize(id, !visible)
    }
}
