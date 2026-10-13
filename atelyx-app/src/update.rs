//! 应用内更新：检查 / 下载 / 安装状态机（纯逻辑）与安装器拉起。
//!
//! 下载经 `atelyx_core::updater`（续传 + sha256）；进度经应用层暂存 + UiTick 刷入常驻通知。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use atelyx_core::updater::ReleaseInfo;

/// 更新流程状态机（Idle → Checking → Available → Downloading → Ready）。
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    /// 有新版本（version = 目标版本号）。
    Available { version: String, download_url: String, sha256: Option<String> },
    /// 下载中（received/total 字节；显示值由 UiTick 从暂存刷入）。
    Downloading { version: String, received: u64, total: u64 },
    /// 下载完成（安装包路径）。
    Ready { version: String, path: PathBuf },
}

impl UpdateState {
    /// 启动静默检查是否放行（autoUpdate 开且当前空闲）。
    pub fn can_auto_check(&self, auto_update: bool) -> bool {
        auto_update && matches!(self, UpdateState::Idle)
    }

    /// 手动/自动检查入口共用的状态迁移（非 Idle 拒绝重入）。
    pub fn start_check(&mut self) -> bool {
        if matches!(self, UpdateState::Idle) {
            *self = UpdateState::Checking;
            true
        } else {
            false
        }
    }

    /// 检查回执：有可用新版本（且本机平台有安装包）进 Available，否则回 Idle。
    /// `current` = 本机版本；Linux 无安装包资产 → Err(页面地址) 由调用方打开 Release 页。
    pub fn finish_check(&mut self, current: &str, release: ReleaseInfo) -> Result<(), String> {
        if atelyx_core::updater::compare_versions(&release.version, current)
            != std::cmp::Ordering::Greater
        {
            *self = UpdateState::Idle;
            return Ok(());
        }
        match (release.asset_url.clone(), release.asset_name.clone()) {
            (Some(url), Some(_)) => {
                *self = UpdateState::Available {
                    version: release.version.clone(),
                    download_url: url,
                    sha256: release.sha256.clone(),
                };
                Ok(())
            }
            _ => {
                *self = UpdateState::Idle;
                Err(release.html_url)
            }
        }
    }

    /// 检查失败回 Idle（静默检查与手动检查共用；错误提示由调用方发）。
    pub fn fail_check(&mut self) {
        *self = UpdateState::Idle;
    }

    /// 开始下载（仅 Available 态可进；返回安装包文件名供通知展示）。
    pub fn start_download(&mut self) -> Option<String> {
        match self {
            UpdateState::Available { version, download_url, .. } => {
                let name = download_url.rsplit('/').next()?.to_string();
                let version = version.clone();
                *self = UpdateState::Downloading { version, received: 0, total: 0 };
                Some(name)
            }
            _ => None,
        }
    }

    /// UiTick 刷入最新进度（仅 Downloading 态收敛）。
    pub fn set_progress(&mut self, received: u64, total: u64) {
        if let UpdateState::Downloading { received: r, total: t, .. } = self {
            *r = received;
            *t = total;
        }
    }

    /// 下载回执：成功进 Ready，失败回 Idle（提示由调用方发）。
    pub fn finish_download(&mut self, result: Result<PathBuf, String>) {
        if let UpdateState::Downloading { version, .. } = self {
            let version = version.clone();
            *self = match result {
                Ok(path) => UpdateState::Ready { version, path },
                Err(_) => UpdateState::Idle,
            };
        }
    }

    /// 取消下载（回 Idle；下载任务经 cancel 标志自行终止）。
    pub fn cancel(&mut self) {
        if matches!(self, UpdateState::Downloading { .. }) {
            *self = UpdateState::Idle;
        }
    }
}

/// 新建下载取消标志。
pub fn cancel_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// 更新流程消息（壳层处理；状态迁移语义在 [`UpdateState`]）。
#[derive(Debug, Clone)]
pub enum UpdateMsg {
    /// 发起检查（manual = 设置页手动触发；静默检查不弹过程与失败提示）。
    Check { manual: bool },
    /// 检查回执（Err = 网络等失败原因）。
    Checked(Result<ReleaseInfo, String>),
    /// 下载 Available 态持有的安装包。
    Download,
    /// 心跳派发：读暂存进度刷入状态与常驻通知（仅 Downloading 态有效）。
    ProgressTick,
    /// 下载回执（Ok = 安装包落位路径；取消后迟到的回执由壳层丢弃）。
    Downloaded(Result<PathBuf, String>),
    /// 取消在途下载（置位取消标志，`.part` 保留供续传）。
    Cancel,
    /// 安装就绪的更新包（脏内容 flush 放行后执行）。
    Install,
}

/// 拉起已下载的安装包（Windows：ShellExecuteW open，能触发 UAC）；调用方随后退出进程让出文件占用。
#[cfg(target_os = "windows")]
pub fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::PCWSTR(verb.as_ptr()),
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // 返回值 > 32 = 成功（HINSTANCE 语义）
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!("安装器拉起失败（代码 {}）", result.0 as usize))
    }
}

/// 拉起 Release 页（Linux 无安装包资产：交给系统浏览器）。
#[cfg(not(target_os = "windows"))]
pub fn launch_installer(_path: &std::path::Path) -> Result<(), String> {
    Err("当前平台不支持应用内安装".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, asset: bool) -> ReleaseInfo {
        ReleaseInfo {
            version: version.to_string(),
            html_url: "https://github.com/Atelyx/Atelyx/releases".to_string(),
            asset_name: asset.then(|| "Atelyx-1.0.0-setup.exe".to_string()),
            asset_url: asset.then(|| "https://example.com/a-setup.exe".to_string()),
            sha256: None,
        }
    }

    #[test]
    fn check_flow() {
        let mut s = UpdateState::Idle;
        assert!(s.start_check());
        assert!(!s.start_check()); // 重入拒绝
        // 新版本 + 有资产 → Available
        s.finish_check("0.1.0", release("0.2.0", true)).unwrap();
        assert!(matches!(s, UpdateState::Available { .. }));
        // 已是最新 → 回 Idle
        s.finish_check("0.2.0", release("0.2.0", true)).unwrap();
        assert_eq!(s, UpdateState::Idle);
        // 新版本但无资产 → Err(页面地址) 且回 Idle
        s.start_check();
        let err = s.finish_check("0.1.0", release("0.3.0", false)).unwrap_err();
        assert_eq!(err, "https://github.com/Atelyx/Atelyx/releases");
        assert_eq!(s, UpdateState::Idle);
    }

    #[test]
    fn download_flow() {
        let mut s = UpdateState::Idle;
        assert_eq!(s.start_download(), None);
        s.start_check();
        s.finish_check("0.1.0", release("0.2.0", true)).unwrap();
        let name = s.start_download().unwrap();
        assert_eq!(name, "a-setup.exe");
        s.set_progress(50, 100);
        assert_eq!(s, UpdateState::Downloading { version: "0.2.0".into(), received: 50, total: 100 });
        // 失败回 Idle
        s.finish_download(Err("网络中断".into()));
        assert_eq!(s, UpdateState::Idle);
        // 成功进 Ready
        s.start_check();
        s.finish_check("0.1.0", release("0.2.0", true)).unwrap();
        s.start_download();
        s.finish_download(Ok(PathBuf::from("C:/tmp/a-setup.exe")));
        assert!(matches!(s, UpdateState::Ready { .. }));
        // Ready 态不再重入下载
        assert_eq!(s.start_download(), None);
    }

    #[test]
    fn auto_check_gating() {
        let s = UpdateState::Idle;
        assert!(s.can_auto_check(true));
        assert!(!s.can_auto_check(false));
        let s = UpdateState::Checking;
        assert!(!s.can_auto_check(true));
    }
}
