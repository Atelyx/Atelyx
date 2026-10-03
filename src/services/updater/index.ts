/**
 * 检查更新 service。
 *
 * 桌面：tauri-plugin-updater + GitHub Release 静态 latest.json，可下载安装并重启（签名校验由插件完成）。
 * 安卓：没有 updater 插件，改为查 GitHub Release API 比对版本，引导用户去下载新 APK 手动安装。
 * 调用时机：设置开启 autoUpdate 后每次启动一次（桌面与安卓共用，App.tsx 挂载后触发）。
 * 启动检查只提示不安装：发现新版本由调用方弹应用内通知，用户点按钮才走 installUpdate。
 * 全链路静默降级：检查失败只记日志，不弹窗、不阻塞工作区，下次启动再试。
 * dev 模式跳过：开发构建未打包签名产物，避免误触发下载安装。
 */
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getAppVersion } from "@/services/app";
import { isAndroidPlatform } from "@/services/platform";
import { openUrl } from "@/services/shell";
import { compareVersions } from "@/utils/pluginManifest";

/** 更新源仓库（与 tauri.conf.json 的 updater.endpoints 同一仓库）。 */
const RELEASE_REPO = "Atelyx/Atelyx";
const RELEASE_API = `https://api.github.com/repos/${RELEASE_REPO}/releases/latest`;
/** 检查超时：弱网下不让启动检查悬挂、也不让关于页一直停在「检查中…」。 */
const CHECK_TIMEOUT_MS = 15_000;

/** 手动检查结果：null = 已是最新版本。 */
export interface UpdateCheckResult {
  latestVersion: string;
}

/** 单次 Release 探测结果（含下载地址；只在本 service 内部流转）。 */
interface ReleaseCheck {
  version: string;
  /** 新版本安装包地址（发布未附 APK 时回落 Release 页面）。 */
  downloadUrl?: string;
}

/** GitHub Release API 用到的字段（只取需要的，其余忽略）。 */
interface GitHubRelease {
  tag_name?: string;
  html_url?: string;
  assets?: { name?: string; browser_download_url?: string }[];
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
  const apk = release.assets?.find((asset) => asset.name?.endsWith(".apk"));
  return { version: tag, downloadUrl: apk?.browser_download_url ?? release.html_url };
}

/** 手动检查新版本（设置页「关于」用）；检查失败抛错交由 UI 展示，null = 已是最新版本。 */
export async function checkForUpdate(): Promise<UpdateCheckResult | null> {
  if (isAndroidPlatform()) {
    const release = await checkRelease();
    return release ? { latestVersion: release.version } : null;
  }
  const update = await check();
  if (!update) return null;
  return { latestVersion: update.version };
}

/** 下载新版本：桌面下载安装并重启；安卓打开下载地址（无 updater，由用户手动安装）。 */
export async function installUpdate(): Promise<void> {
  if (isAndroidPlatform()) {
    const release = await checkRelease();
    if (!release) throw new Error("已是最新版本");
    if (!release.downloadUrl) throw new Error("发布包中没有找到可下载的安装包");
    await openUrl(release.downloadUrl);
    return;
  }
  const update = await check();
  if (!update) return;
  await update.downloadAndInstall();
  await relaunch();
}

/** 启动静默检查（桌面与安卓共用，只看不装）：dev 跳过；失败静默降级，由调用方决定如何提示。 */
export async function checkUpdateOnStartup(): Promise<UpdateCheckResult | null> {
  if (import.meta.env.DEV) return null;
  try {
    if (isAndroidPlatform()) {
      const release = await checkRelease();
      return release ? { latestVersion: release.version } : null;
    }
    const update = await check();
    return update ? { latestVersion: update.version } : null;
  } catch (e) {
    console.error("检查更新失败（静默降级，下次启动再试）", e);
    return null;
  }
}
