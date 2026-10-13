//! 开机自启：真源 = 系统启动项（Windows Run 注册表 / Linux XDG autostart），不落应用配置。

/// 读系统启动项当前是否已注册本应用（读失败按未注册展示，不禁用开关）。
pub fn enabled() -> bool {
    sys::enabled()
}

/// 注册/注销开机自启（幂等）；失败返回原因，由调用方提示并回读系统真实状态。
pub fn set_enabled(on: bool) -> Result<(), String> {
    sys::set(on)
}

#[cfg(windows)]
mod sys {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "Atelyx";

    pub fn enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(RUN_KEY)
            .and_then(|key| key.get_raw_value(VALUE_NAME).map(|_| ()))
            .is_ok()
    }

    pub fn set(on: bool) -> Result<(), String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .map_err(|e| format!("打开注册表启动项失败：{e}"))?;
        if on {
            let exe = std::env::current_exe().map_err(|e| format!("定位程序路径失败：{e}"))?;
            // Run 键值按命令行解析，含空格路径必须加引号
            let quoted = format!("\"{}\"", exe.to_string_lossy());
            key.set_value(VALUE_NAME, &quoted)
                .map_err(|e| format!("写入注册表启动项失败：{e}"))
        } else {
            match key.delete_value(VALUE_NAME) {
                // 值不存在 = 注销完成（幂等）
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除注册表启动项失败：{e}")),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    const DESKTOP_DIR: &str = "autostart";
    const DESKTOP_NAME: &str = "atelyx.desktop";

    fn desktop_path() -> Result<std::path::PathBuf, String> {
        dirs::config_dir()
            .map(|d| d.join(DESKTOP_DIR).join(DESKTOP_NAME))
            .ok_or_else(|| "无法定位用户配置目录".to_string())
    }

    pub fn enabled() -> bool {
        desktop_path().is_ok_and(|p| p.exists())
    }

    pub fn set(on: bool) -> Result<(), String> {
        let path = desktop_path()?;
        if !on {
            return match std::fs::remove_file(&path) {
                // 文件不存在 = 注销完成（幂等）
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除自启文件失败：{e}")),
            };
        }
        let exe = std::env::current_exe().map_err(|e| format!("定位程序路径失败：{e}"))?;
        let exe = exe.to_string_lossy().into_owned();
        // Desktop Entry 规范：Exec 含空格的路径须加引号
        let exec = if exe.contains(' ') { format!("\"{exe}\"") } else { exe };
        let content = format!("[Desktop Entry]\nType=Application\nExec={exec}\nName=Atelyx\n");
        let dir = path.parent().ok_or_else(|| "自启文件路径异常".to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("创建自启目录失败：{e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("写入自启文件失败：{e}"))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod sys {
    pub fn enabled() -> bool {
        false
    }

    pub fn set(_on: bool) -> Result<(), String> {
        Err("当前平台不支持开机自启".to_string())
    }
}
