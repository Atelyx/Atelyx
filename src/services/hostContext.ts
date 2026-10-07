/**
 * 打开文件上下文宿主（单进程真源）：Rust 持有跨窗口协调态，主窗口唯一写者（command 写入 +
 * Rust 广播），撕裂窗口是薄客户端（boot command 拉基线 + 订阅广播增量），窗口间不再互相广播。
 * 收敛语义：基线快照晚于任何先前广播，基线与增量按任意到达序应用都收敛到同一状态。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { VaultIdentity } from "@/services/content/contract";

/** 当前仓库/打开文件上下文（Rust 宿主持有；撕裂窗口据此镜像上下文与自建内容后端）。 */
export interface OpenFileContext {
  vaultRoot: string | null;
  /** 当前激活仓库身份（local/space；null = 未激活）。撕裂窗口是独立 webview，
   *  内容面激活态不跨窗口共享，据此自建对应后端（见 factory.activateContentIdentity）。 */
  vaultIdentity: VaultIdentity | null;
  vaultName: string;
  currentCanvasFile: string | null;
  currentNoteFile: string | null;
  currentTableFile: string | null;
  currentNoteTitle: string;
  currentTableTitle: string;
}

/** 读取宿主持有的当前上下文快照（null = 宿主尚未播种，等广播即可）。 */
export function getOpenFileContext(): Promise<OpenFileContext | null> {
  return invoke<OpenFileContext | null>("get_open_file_context");
}

/** 写入上下文（主窗口唯一写者；Rust 存真源并向全部窗口广播，含本窗口）。 */
export function setOpenFileContext(context: OpenFileContext): Promise<void> {
  return invoke("set_open_file_context", { context });
}

/** 订阅上下文广播（撕裂窗口各订一份；载荷为整体快照，幂等应用）。 */
export function onOpenFileContextChanged(
  handler: (context: OpenFileContext) => void,
): Promise<UnlistenFn> {
  return listen<OpenFileContext>("open-file-context-changed", (e) => handler(e.payload));
}
