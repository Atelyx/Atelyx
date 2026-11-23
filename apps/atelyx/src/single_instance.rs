//! 单实例：第二个进程只把已在运行的窗口唤出来再退出，避免多份进程抢托盘与写盘。
//! Windows 用命名互斥锁判重、向托盘隐藏窗口投唤出消息；其余平台没有托盘入口，不做单实例。

/// 本进程是否应继续启动（false = 已运行实例已被唤出，调用方直接退出）。
pub fn ensure_single() -> bool {
    platform::ensure_single()
}

/// 唤出消息名（托盘窗口接收）。
#[cfg(windows)]
pub(crate) use platform::SHOW_MESSAGE;

#[cfg(not(windows))]
mod platform {
    /// 托盘只在 Windows 实现，没有可唤回的窗口入口。
    pub(super) fn ensure_single() -> bool {
        true
    }
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, LPARAM, WPARAM,
    };
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, PostMessageW, RegisterWindowMessageW,
    };

    /// 实例锁名（会话内唯一；进程退出即释放）。
    const MUTEX_NAME: &str = "Local\\Atelyx-SingleInstance";
    /// 唤出消息名（已运行实例的托盘窗口接收）。
    pub(crate) const SHOW_MESSAGE: &str = "AtelyxShowMain";
    /// 等已运行实例把托盘窗口建好的重试次数与间隔（首个实例可能还在启动）。
    const FIND_ATTEMPTS: usize = 20;
    const FIND_INTERVAL: Duration = Duration::from_millis(100);

    pub(super) fn ensure_single() -> bool {
        let name = wide(MUTEX_NAME);
        // CreateMutex 只在「已存在」时置 ERROR_ALREADY_EXISTS，先清零上次错误才判得准
        unsafe { SetLastError(ERROR_SUCCESS) };
        let handle = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
            Ok(handle) => handle,
            // 建锁失败（资源不足 / 权限）：不拦启动，按无实例处理
            Err(_) => return true,
        };
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            // 首个实例：句柄不关闭（HANDLE 是裸指针，没有析构），锁随进程结束由系统释放
            let _ = handle;
            return true;
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        wake_existing();
        false
    }

    /// 找到已运行实例的托盘窗口投一条唤出消息；窗口还没建好就短暂重试。
    fn wake_existing() {
        let class = wide(crate::tray::WINDOW_CLASS);
        let message = wide(SHOW_MESSAGE);
        let msg = unsafe { RegisterWindowMessageW(PCWSTR(message.as_ptr())) };
        if msg == 0 {
            // 注册失败拿不到有效消息 id：实例锁已让本进程退出，不再尝试唤醒
            return;
        }
        for _ in 0..FIND_ATTEMPTS {
            if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) } {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
                }
                return;
            }
            std::thread::sleep(FIND_INTERVAL);
        }
        // 找不到窗口（旧实例托盘建不起来）：实例锁已让本进程退出，不再继续找
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
