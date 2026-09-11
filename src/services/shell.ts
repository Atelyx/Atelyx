/**
 * 系统 shell service：在文件管理器中打开路径 / 默认程序打开 URL / 执行外部进程（流式）。
 *
 * - `open`（openInExplorer / openUrl）：放行范围由 `tauri.conf.json > plugins > shell > open`
 *   的正则决定（该正则由 tauri-plugin-shell 整体包上 `^...$` 后逐项匹配），
 *   不匹配即 `Err(Validation)`；本地路径依赖该配置放行。
 * - 进程执行：经 tauri-plugin-shell 的 Command API，要过两道关——权限点
 *   （`shell:allow-spawn`，capability 里授予）与**程序 scope**（capability 里逐个登记
 *   `name`/`cmd`，无通配符）。因此本模块传的 `program` 必须等于 scope 登记的 `name`：
 *   `capabilities/shell-exec-unix.json` 登记 `sh`、`shell-exec-windows.json` 登记 `cmd.exe`，
 *   两者 `args` 全开（插件传 `-c`/`/C <命令>` 即等价任意命令执行）。传未登记的程序会被拒绝。
 */
import { open as shellOpen, Command } from "@tauri-apps/plugin-shell";

/** 在系统文件管理器中打开路径（目录或文件，选中态）。 */
export async function openInExplorer(path: string): Promise<void> {
  await shellOpen(path);
}

/** 用系统默认程序打开外部 URL（http/https/mailto 等；webview 不导航）。 */
export async function openUrl(url: string): Promise<void> {
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

/** 执行外部进程（流式 stdout/stderr + 退出回调）；返回 cancel（kill 进程）。 */
export function runProcess(
  program: string,
  args: string[],
  options: RunProcessOptions,
  handlers: ProcessStreamHandlers,
): { cancel: () => void } {
  const command = Command.create(program, args, { cwd: options.cwd, env: options.env });
  command.stdout.on("data", (line: string) => handlers.stdout(line));
  command.stderr.on("data", (line: string) => handlers.stderr(line));
  command.on("close", (e: { code: number | null; signal: number | null }) => handlers.close(e.code));
  command.on("error", (e: string) => handlers.error(String(e)));
  const childPromise = command.spawn();
  void childPromise.catch((e) => handlers.error(e instanceof Error ? e.message : String(e)));
  return {
    cancel: () => {
      void childPromise
        .then((child) => child.kill())
        .catch(() => {});
    },
  };
}
