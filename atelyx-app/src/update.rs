//! 应用内更新：检查 / 下载 / 安装状态机（纯逻辑）与安装器拉起。
//!
//! 下载经 `atelyx_core::updater`（续传 + sha256）；进度经应用层暂存 + UiTick 刷入常驻通知。

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use atelyx_core::updater::{compare_versions, ReleaseInfo};

/// 运行期静默检查的轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// 一次检查对用户可见的结果（状态迁移已在 [`UpdateState::apply_check`] 内完成）。
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// 有可下载的新版本。
    Available { version: String },
    /// 有新版本但本机平台无安装包；`page` 为发布页地址。
    NoAsset { version: String, page: String },
    /// 已是最新版本（自动检查不提示）。
    UpToDate,
}

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
    /// 自动检查是否放行：开关开启且不在下载 / 已就绪；已有可下载版本时仍轮询以发现更高版本。
    pub fn can_auto_check(&self, auto_update: bool) -> bool {
        auto_update && matches!(self, UpdateState::Idle | UpdateState::Available { .. })
    }

    /// 手动检查入口的状态迁移（非 Idle 拒绝重入）。
    pub fn start_check(&mut self) -> bool {
        if matches!(self, UpdateState::Idle) {
            *self = UpdateState::Checking;
            true
        } else {
            false
        }
    }

    /// 检查回执合并。`manual` = 用户显式发起（无视本次运行的跳过记录）；自动检查不重复提示已在手
    /// 的版本、也不为无安装包的更高版本打扰可下载版本。返回需要告知用户的结果；None = 静默。
    /// 未能产生可下载版本的结果都会把「检查中」收敛回空闲，已在手的可下载版本保持不变。
    pub fn apply_check(
        &mut self,
        current: &str,
        skipped: Option<&str>,
        release: ReleaseInfo,
        manual: bool,
    ) -> Option<CheckOutcome> {
        // 检查在途时用户开始了下载 / 已有安装包：回执不得覆盖进行中的流程
        if matches!(self, UpdateState::Downloading { .. } | UpdateState::Ready { .. }) {
            return None;
        }
        let ReleaseInfo { version, html_url, asset_name, asset_url, sha256 } = release;
        let newer = compare_versions(&version, current) == Ordering::Greater;
        let known = match self {
            UpdateState::Available { version, .. } => Some(version.clone()),
            _ => None,
        };
        let outcome = if !newer {
            manual.then_some(CheckOutcome::UpToDate)
        } else if (!manual && skipped == Some(version.as_str()))
            || known
                .as_deref()
                .is_some_and(|known| compare_versions(known, &version) != Ordering::Less)
        {
            // 已跳过的版本不再自动提示；已在手的版本不重复提示，也不回退到更旧的版本
            None
        } else if let Some(url) = asset_url.filter(|_| asset_name.is_some()) {
            *self = UpdateState::Available {
                version: version.clone(),
                download_url: url,
                sha256,
            };
            Some(CheckOutcome::Available { version })
        } else if known.is_some() {
            None
        } else {
            Some(CheckOutcome::NoAsset { version, page: html_url })
        };
        if !matches!(self, UpdateState::Available { .. }) {
            *self = UpdateState::Idle;
        }
        outcome
    }

    /// 检查失败回原态：检查中 → 空闲；自动轮询失败不打断已发现的可下载版本。
    pub fn fail_check(&mut self) {
        if matches!(self, UpdateState::Checking) {
            *self = UpdateState::Idle;
        }
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
    /// 跳过通知里的版本（点「跳过」或通知 ×）：本次运行不再自动提示该版本。
    Skip { version: String },
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

    const PAGE: &str = "https://github.com/Atelyx/Atelyx/releases";

    fn available(version: &str) -> UpdateState {
        UpdateState::Available {
            version: version.to_string(),
            download_url: "https://example.com/a-setup.exe".to_string(),
            sha256: None,
        }
    }

    #[test]
    fn manual_check_flow() {
        let mut s = UpdateState::Idle;
        assert!(s.start_check());
        assert!(!s.start_check()); // 重入拒绝
        // 新版本 + 有资产 → Available
        assert_eq!(
            s.apply_check("0.1.0", None, release("0.2.0", true), true),
            Some(CheckOutcome::Available { version: "0.2.0".into() })
        );
        assert!(matches!(s, UpdateState::Available { .. }));
        // 已是最新 → UpToDate 且回 Idle
        s = UpdateState::Idle;
        s.start_check();
        assert_eq!(
            s.apply_check("0.2.0", None, release("0.2.0", true), true),
            Some(CheckOutcome::UpToDate)
        );
        assert_eq!(s, UpdateState::Idle);
        // 新版本但无资产 → NoAsset + 页面地址，状态回 Idle
        s.start_check();
        assert_eq!(
            s.apply_check("0.1.0", None, release("0.3.0", false), true),
            Some(CheckOutcome::NoAsset { version: "0.3.0".into(), page: PAGE.into() })
        );
        assert_eq!(s, UpdateState::Idle);
    }

    #[test]
    fn auto_check_skips_and_replaces() {
        // 已跳过的版本静默
        let mut s = UpdateState::Idle;
        assert_eq!(
            s.apply_check("0.1.0", Some("0.2.0"), release("0.2.0", true), false),
            None
        );
        assert_eq!(s, UpdateState::Idle);
        // 手动检查无视跳过记录
        s.start_check();
        assert_eq!(
            s.apply_check("0.1.0", Some("0.2.0"), release("0.2.0", true), true),
            Some(CheckOutcome::Available { version: "0.2.0".into() })
        );
        // 已在手的版本：自动轮询不重复提示
        assert_eq!(s.apply_check("0.1.0", None, release("0.2.0", true), false), None);
        assert_eq!(s, available("0.2.0"));
        // 更高版本替换状态并提示
        assert_eq!(
            s.apply_check("0.1.0", None, release("0.3.0", true), false),
            Some(CheckOutcome::Available { version: "0.3.0".into() })
        );
        assert_eq!(s, available("0.3.0"));
        // 晚到的更旧结果不回退
        assert_eq!(s.apply_check("0.1.0", None, release("0.2.5", true), false), None);
        assert_eq!(s, available("0.3.0"));
        // 无安装包的更高版本不打扰已有可下载版本
        assert_eq!(s.apply_check("0.1.0", None, release("0.4.0", false), false), None);
        assert_eq!(s, available("0.3.0"));
    }

    #[test]
    fn auto_check_without_asset_reports_page() {
        let mut s = UpdateState::Idle;
        assert_eq!(
            s.apply_check("0.1.0", None, release("0.2.0", false), false),
            Some(CheckOutcome::NoAsset { version: "0.2.0".into(), page: PAGE.into() })
        );
        assert_eq!(s, UpdateState::Idle);
    }

    #[test]
    fn download_flow() {
        let mut s = UpdateState::Idle;
        assert_eq!(s.start_download(), None);
        s.start_check();
        s.apply_check("0.1.0", None, release("0.2.0", true), true);
        let name = s.start_download().unwrap();
        assert_eq!(name, "a-setup.exe");
        s.set_progress(50, 100);
        assert_eq!(s, UpdateState::Downloading { version: "0.2.0".into(), received: 50, total: 100 });
        // 失败回 Idle
        s.finish_download(Err("网络中断".into()));
        assert_eq!(s, UpdateState::Idle);
        // 成功进 Ready
        s.start_check();
        s.apply_check("0.1.0", None, release("0.2.0", true), true);
        s.start_download();
        s.finish_download(Ok(PathBuf::from("C:/tmp/a-setup.exe")));
        assert!(matches!(s, UpdateState::Ready { .. }));
        // Ready 态不再重入下载
        assert_eq!(s.start_download(), None);
    }

    /// 下载 / 已就绪途中到达的检查回执不得覆盖进行中的流程。
    #[test]
    fn check_result_does_not_clobber_download() {
        let mut s = UpdateState::Idle;
        s.start_check();
        s.apply_check("0.1.0", None, release("0.2.0", true), true);
        s.start_download();
        assert_eq!(s.apply_check("0.1.0", None, release("0.3.0", true), false), None);
        assert!(matches!(&s, UpdateState::Downloading { version, .. } if version == "0.2.0"));
        s.finish_download(Ok(PathBuf::from("C:/tmp/a-setup.exe")));
        assert_eq!(s.apply_check("0.1.0", None, release("0.3.0", true), false), None);
        assert!(matches!(s, UpdateState::Ready { version, .. } if version == "0.2.0"));
    }

    /// 自动轮询失败不得把已发现的可下载版本清空；检查中失败才回空闲。
    #[test]
    fn fail_check_preserves_available() {
        let mut s = UpdateState::Idle;
        s.start_check();
        s.apply_check("0.1.0", None, release("0.2.0", true), true);
        s.fail_check();
        assert_eq!(s, available("0.2.0"));
        s = UpdateState::Checking;
        s.fail_check();
        assert_eq!(s, UpdateState::Idle);
    }

    #[test]
    fn auto_check_gating() {
        assert!(UpdateState::Idle.can_auto_check(true));
        assert!(!UpdateState::Idle.can_auto_check(false));
        assert!(available("0.2.0").can_auto_check(true));
        assert!(!UpdateState::Checking.can_auto_check(true));
        let downloading = UpdateState::Downloading { version: "0.2.0".into(), received: 0, total: 1 };
        assert!(!downloading.can_auto_check(true));
        let ready = UpdateState::Ready { version: "0.2.0".into(), path: PathBuf::from("p") };
        assert!(!ready.can_auto_check(true));
    }
}
