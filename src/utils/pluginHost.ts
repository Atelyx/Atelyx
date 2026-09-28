/**
 * 插件宿主环境探测（纯函数）。
 * 平台字符串与清单 `platforms` 取值对齐（windows-x64 / linux-x64 / android）；未知平台返回
 * "unknown"（安装时平台过滤对 unknown 不生效，由版本范围兜底）。
 * 安卓 UA 形如 `(Linux; Android 15; …)`，必须先于 linux 判定，否则会把安卓误判成 linux-x64。
 */
export const ANDROID_PLATFORM = "android";

export function detectPlatform(): string {
  const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
  if (/android/i.test(ua)) return ANDROID_PLATFORM;
  if (/windows/i.test(ua)) return "windows-x64";
  if (/linux/i.test(ua)) return "linux-x64";
  return "unknown";
}
