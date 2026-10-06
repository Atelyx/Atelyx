/**
 * 系统 shell service：在文件管理器中打开路径 / 默认程序打开 URL / 执行外部进程（流式）。
 * `open` 的放行范围由 `tauri.conf.json > plugins > shell > open` 正则决定（该正则被整体包上 `^...$`
 * 后逐项匹配），不匹配即 `Err(Validation)`——本地路径也依赖该配置放行。
 */
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { invoke, Channel } from "@tauri-apps/api/core";
import { isAndroidPlatform } from "@/services/platform";
import { openExternalUrl } from "@/services/mobilePlatform";

/** 在系统文件管理器中打开路径（目录或文件，选中态）。 */
export async function openInExplorer(path: string): Promise<void> {
  // 安卓没有文件管理器概念（应用私有目录不可见、共享存储无「选中」语义），显式拒绝不静默
  if (isAndroidPlatform()) throw new Error("当前平台不支持在文件管理器中打开");
  await shellOpen(path);
}

/** 用系统默认程序打开外部 URL（http/https/mailto 等；webview 不导航）。
 *  安卓端 shell 插件未注册（桌面专有），走原生 Intent。 */
export async function openUrl(url: string): Promise<void> {
  if (isAndroidPlatform()) {
    await openExternalUrl(url);
    return;
  }
  await shellOpen(url);
}

/** 进程输出处理器（流式）。 */
export interface ProcessStreamHandlers {
  stdout(line: string): void;
  stderr(line: string): void;
  /** 进程正常退出（code 为退出码；被信号终止时为 null，Unix）。 */
  close(code: number | null): void;
  /** spawn/运行期错误。 */
  error(message: string): void;
}

export interface RunProcessOptions {
  cwd?: string;
  env?: Record<string, string>;
}

/** 后端进程事件（与 `src-tauri/src/plugin_process.rs` 的 `ProcessEvent` 逐字对齐）。 */
type ProcessEvent =
  | { event: "stdout"; data: string }
  | { event: "stderr"; data: string }
  | { event: "terminated"; code: number | null }
  | { event: "error"; message: string };

/** 执行外部进程（流式 stdout/stderr + 退出回调）；返回进程 pid（spawn 成功后 resolve）。
 *
 *  走宿主命令 `spawn_plugin_process`（程序白名单在 Rust 侧，只放行 `sh`/`cmd.exe`，参数全开）：进程在创建
 *  那一刻就被纳入清理范围（Windows 作业对象 / Unix 进程组），否则 `sh -c` 派生的真服务会漏在清理之外。
 *  pid 是结束进程的唯一可靠凭据（按命令行特征匹配会因启动方式变化失效，且有误杀同目录进程的风险），
 *  故交给调用方记账。启动失败（程序不在白名单等）reject，同时经 `error` 上报同因错误。 */
export function runProcess(
  program: string,
  args: string[],
  options: RunProcessOptions,
  handlers: ProcessStreamHandlers,
): Promise<number> {
  const onEvent = new Channel<ProcessEvent>();
  onEvent.onmessage = (payload) => {
    switch (payload.event) {
      case "stdout":
        handlers.stdout(payload.data);
        break;
      case "stderr":
        handlers.stderr(payload.data);
        break;
      case "terminated":
        handlers.close(payload.code);
        break;
      case "error":
        handlers.error(payload.message);
        break;
    }
  };
  const started = invoke<number>("spawn_plugin_process", {
    program,
    args,
    cwd: options.cwd ?? null,
    env: options.env ?? null,
    onEvent,
  });
  // 启动失败同时反映到返回的 promise（调用方 await 得到失败原因）与 error 回调（流式面同口径）
  void started.catch((e) => handlers.error(e instanceof Error ? e.message : String(e)));
  return started;
}

/** 结束 pid 及其全部子孙进程（已不存在 = 成功；其余失败 reject 带原因）。
 * 走 `kill_process_tree` 命令整棵树结束，不用 `Child.kill`：插件起的服务被 `sh -c`/`cmd.exe /C` 包一层，
 * 只杀包装进程会把真服务留成孤儿。 */
export function killProcessTree(pid: number): Promise<void> {
  return invoke<void>("kill_process_tree", { pid });
}
