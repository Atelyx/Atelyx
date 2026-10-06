/**
 * 开机自启 service：真相源 = 系统启动项（Windows Run 注册表 / XDG autostart / LaunchAgent），
 * 状态每次实时读系统、不落 global.json（防用户从系统侧改动后应用显示与之漂移）。
 * 调用时机：应用挂载读一次，通用设置面板打开时刷新一次。移动端无 autostart 插件，统一 no-op。
 */
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { isAndroidPlatform } from "@/services/platform";

/** 读取系统启动项当前是否已注册本应用。 */
export async function isAutoLaunchEnabled(): Promise<boolean> {
  if (isAndroidPlatform()) return false;
  return isEnabled();
}

/** 注册/注销开机自启（幂等）；失败抛错，由调用方提示并回读系统真实状态。 */
export async function setAutoLaunchEnabled(enabled: boolean): Promise<void> {
  if (isAndroidPlatform()) return;
  if (enabled) await enable();
  else await disable();
}
