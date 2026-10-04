/**
 * 检查更新与安装 service。
 *
 * 三端统一查 GitHub Release API 比对版本：Windows/安卓应用内下载（进度/取消/续传/sha256）后拉起
 * 系统安装器；Linux 发行版安装语义各异，改为打开 Release 下载页。
 * 启动检查只提示不安装；失败静默降级（下次启动再试）；dev 跳过。
 */
import { Channel, invoke } from "@tauri-apps/api/core";
import { getAppVersion } from "@/services/app";
import { isAndroidPlatform, platformCapabilities } from "@/services/platform";
import { openUrl } from "@/services/shell";
import { compareVersions } from "@/utils/pluginManifest";

/** 更新源仓库（与 tauri.conf.json 的发布产物同一仓库）。 */
const RELEASE_REPO = "Atelyx/Atelyx";
const RELEASE_API = `https://api.github.com/repos/${RELEASE_REPO}/releases/latest`;
/** 检查超时：弱网下不让启动检查悬挂、也不让关于页一直停在「检查中…」。 */
const CHECK_TIMEOUT_MS = 15_000;

/** 手动检查结果：null = 已是最新版本。 */
export interface UpdateCheckResult {
  latestVersion: string;
}

/** 安装流程结果：cancelled = 用户取消下载（可重新开始）；started = 已交给安装器或下载页。 */
export type InstallOutcome = "started" | "cancelled";

/** 安装过程中的回调：进度用于界面展示，onBeforeInstall 用于安装前落盘。 */
export interface InstallUpdateHandlers {
  /** 已下载字节与总字节（总字节未知时为 null）。 */
  onProgress?(received: number, total: number | null): void;
  /** 下载完成，正在校验摘要。 */
  onVerifying?(): void;
  /** **拉起安装器之前**落盘 pending 改动；由调用方注入（service 不依赖 store）。 */
  onBeforeInstall?(): Promise<void>;
}

/** 本平台对应的更新安装包（Linux 无应用内安装，只有下载页）。 */
interface UpdateAsset {
  fileName: string;
  url: string;
  /** GitHub 提供的 sha256 摘要（`sha256:<hex>`）；缺失时 Rust 侧跳过校验。 */
  sha256: string | null;
}

/** 单次 Release 探测结果（只在本 service 内部流转）。 */
interface ReleaseCheck {
  version: string;
  /** 可应用内安装的产物；null = 交回下载页。 */
  asset: UpdateAsset | null;
  /** 发布页地址（Linux 打开它；缺安装包时作兜底）。 */
  releaseUrl: string;
}

/** GitHub Release API 用到的字段（只取需要的，其余忽略）。 */
interface GitHubRelease {
  tag_name?: string;
  html_url?: string;
  assets?: { name?: string; browser_download_url?: string; digest?: string }[];
}

/** 后端下载进度事件（与 `src-tauri/src/commands/update.rs` 的 `DownloadEvent` 逐字对齐）。 */
type DownloadEvent =
  | { event: "started"; total: number | null; resumingFrom: number }
  | { event: "progress"; received: number; total: number | null }
  | { event: "verifying" };

/** 本平台所需安装包名的后缀；null = 本平台不做应用内安装（改为打开下载页）。 */
function assetSuffix(): string | null {
  if (!platformCapabilities().inAppUpdate) return null;
  return isAndroidPlatform() ? ".apk" : "-setup.exe";
}

/** 查 GitHub Release 并比对本地版本；无更新返回 null，检查失败抛错。 */
async function checkRelease(): Promise<ReleaseCheck | null> {
  const res = await fetch(RELEASE_API, {
    headers: { Accept: "application/vnd.github+json" },
    signal: AbortSignal.timeout(CHECK_TIMEOUT_MS),
  });
  if (!res.ok) throw new Error(`检查更新失败（HTTP ${res.status}）`);
  const release = (await res.json()) as GitHubRelease;
  const tag = (release.tag_name ?? "").trim().replace(/^v/i, "");
  if (!tag) throw new Error("检查更新失败：发布信息缺少版本号");
  if (compareVersions(tag, await getAppVersion()) <= 0) return null;

  const suffix = assetSuffix();
  const matched = suffix
    ? release.assets?.find((item) => item.name?.toLowerCase().endsWith(suffix))
    : undefined;
  return {
    version: tag,
    releaseUrl: release.html_url ?? `https://github.com/${RELEASE_REPO}/releases`,
    asset:
      matched?.name && matched.browser_download_url
        ? {
            fileName: matched.name,
            url: matched.browser_download_url,
            // 旧发布不带 digest：缺失即跳过校验
            sha256: matched.digest ?? null,
          }
        : null,
  };
}

/** 手动检查新版本（设置页「关于」用）；检查失败抛错交由 UI 展示，null = 已是最新版本。 */
export async function checkForUpdate(): Promise<UpdateCheckResult | null> {
  const release = await checkRelease();
  return release ? { latestVersion: release.version } : null;
}

/** 启动静默检查（只看不装）：dev 跳过；失败静默降级，由调用方决定如何提示。 */
export async function checkUpdateOnStartup(): Promise<UpdateCheckResult | null> {
  if (import.meta.env.DEV) return null;
  try {
    const release = await checkRelease();
    return release ? { latestVersion: release.version } : null;
  } catch (e) {
    console.error("检查更新失败（静默降级，下次启动再试）", e);
    return null;
  }
}

/** 应用内下载更新包；返回落盘路径，用户取消时返回 null（`.part` 保留，重下即续传）。 */
async function downloadAsset(
  asset: UpdateAsset,
  handlers: InstallUpdateHandlers,
): Promise<string | null> {
  const channel = new Channel<DownloadEvent>();
  channel.onmessage = (payload) => {
    if (payload.event === "started") handlers.onProgress?.(payload.resumingFrom, payload.total);
    else if (payload.event === "progress") handlers.onProgress?.(payload.received, payload.total);
    else handlers.onVerifying?.();
  };
  return invoke<string | null>("download_update_package", {
    url: asset.url,
    fileName: asset.fileName,
    sha256: asset.sha256,
    onEvent: channel,
  });
}

/** 取消在途下载（只在 Windows/安卓的应用内下载期间有意义）。 */
export function cancelUpdateDownload(): Promise<void> {
  return invoke<void>("cancel_update_download");
}

/**
 * 下载并安装新版本。
 *
 * 落盘须在**下载完成后、安装之前**：下载可能持续数分钟，提前落盘会丢掉期间的编辑。
 */
export async function installUpdate(
  handlers: InstallUpdateHandlers = {},
): Promise<InstallOutcome> {
  const release = await checkRelease();
  if (!release) throw new Error("已是最新版本");
  if (!release.asset) {
    // 无应用内安装产物（Linux，或该平台漏传安装包）：交回下载页
    await openUrl(release.releaseUrl);
    return "started";
  }

  const android = isAndroidPlatform();
  // 未授权「安装未知应用」时安装意图会被系统丢弃：先要授权，再下几十 MB
  if (android && !(await invoke<boolean>("android_can_install_packages"))) {
    await invoke<void>("android_request_install_permission");
    throw new Error("请在「安装未知应用」里允许本应用，然后重新点下载");
  }

  const path = await downloadAsset(release.asset, handlers);
  if (!path) return "cancelled";

  await handlers.onBeforeInstall?.();
  if (android) await invoke<void>("android_install_apk", { path });
  else await invoke<void>("install_downloaded_update", { path });
  return "started";
}
