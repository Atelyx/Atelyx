/**
 * 跨窗口写盘感知桥（本地仓库）：把 Rust 在写命令成功后广播的内容变更（`vault-content-changed`，
 * 排除发起窗口）分发到各内容域的既有收敛机制，打开中的文件据此与磁盘对账。
 * 各域「状态如何跟上」的决策见 noteWritten/canvasWritten/tableWritten/renamed；协作空间内容
 * 真源在服务端，其他窗口的变化经协作通道到达，不经本桥。
 */
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getActiveVaultIdentity } from "@/services/content/factory";
import { remapDirPrefix } from "@/utils/filename";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useTableStore } from "@/stores/tableStore";
import { useNoteStore } from "@/stores/noteStore";
import { openNoteSessionFiles } from "@/stores/noteSessionStore";
import { adoptRemoteNoteRelocate } from "@/stores/noteCollabStore";

/** 一条内容变更（相对仓库根路径）。 */
type ContentWriteChange =
  | { kind: "write"; file: string; from?: string }
  | { kind: "rename"; from: string; file: string };

/** `vault-content-changed` 载荷。 */
export interface VaultContentChangedPayload {
  /** 发起窗口 label：订阅回调据此跳过本窗口自己的写盘（域内收敛已有既有信号）。 */
  origin: string;
  /** 仓库根（本地绝对路径）：与本窗口激活身份不符的广播丢弃（切换在途的迟到事件防串仓）。 */
  root: string;
  changes: ContentWriteChange[];
}

/** 路径扩展名（小写；目录名无扩展名返回空串）。 */
function extOf(path: string): string {
  const base = path.split("/").pop() ?? path;
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/** 画布路径跟随：同步 canvasFile 与 appStore 的当前画布（同源，与本地保存收尾一致）。 */
function followCanvasPath(oldFile: string, newFile: string): void {
  useCanvasStore.setState({ canvasFile: newFile });
  if (useAppStore.getState().currentCanvasFile === oldFile) {
    useAppStore.setState({ currentCanvasFile: newFile });
  }
}

/** 表格路径跟随：同步 tableFile 与 appStore 的当前表格（同源，与本地保存收尾一致）。 */
function followTablePath(oldFile: string, newFile: string): void {
  useTableStore.setState({ tableFile: newFile });
  if (useAppStore.getState().currentTableFile === oldFile) {
    useAppStore.setState({ currentTableFile: newFile });
  }
}

/** 画布干净 = 无未落盘编辑且无在途写盘：此时重读磁盘不会吞掉本地改动。 */
function canvasClean(): boolean {
  const s = useCanvasStore.getState();
  return !s.dirty && !s.saving;
}

/** 表格干净判定（语义同画布）。 */
function tableClean(): boolean {
  const s = useTableStore.getState();
  return !s.dirty && !s.saving;
}

/** .md 落盘：作废内容缓存 + bump 外部变更序号（编辑会话收敛语义见 markNoteExternallyEdited），打开中的画布对引用该文件的 text 节点补读。 */
function noteWritten(file: string): void {
  useNoteStore.getState().markNoteExternallyEdited(file);
  useNoteStore.getState().invalidateNoteCache(file);
  const canvas = useCanvasStore.getState();
  if (canvas.canvasFile !== null) void canvas.refreshTextContent(file);
}

/** .atlx 落盘：打开中的画布先跟路径（title 漂移/移动），干净态重读磁盘；脏或在途写盘则保留本地，由下一次自动保存按各自语义落盘（补丁按稳定 id 合并、整写后写者胜）。 */
function canvasWritten(file: string, from: string | undefined): void {
  const canvas = useCanvasStore.getState();
  if (canvas.canvasFile === null) return;
  if (from !== undefined && canvas.canvasFile === from) {
    followCanvasPath(from, file);
  } else if (canvas.canvasFile !== file) {
    return;
  }
  if (canvasClean()) void useCanvasStore.getState().reloadFromDisk();
}

/** .atb 落盘：语义同画布（先跟路径，干净态重读；脏态保留本地待自动保存落盘）。 */
function tableWritten(file: string, from: string | undefined): void {
  const table = useTableStore.getState();
  if (table.tableFile === null) return;
  if (from !== undefined && table.tableFile === from) {
    followTablePath(from, file);
  } else if (table.tableFile !== file) {
    return;
  }
  if (tableClean()) void useTableStore.getState().reloadFromDisk();
}

/**
 * 路径迁移（改名/移动，含目录改名）：打开中的笔记按协作换路簿记跟上；
 * 打开中的画布/表格路径跟上后，干净态重读（迁移方已改写文件内 title/引用，重读对齐）。
 * 目录改名 = 前缀映射：笔记按前缀换路，画布/表格按前缀跟路径。
 */
function renamed(from: string, file: string): void {
  const ext = extOf(file) || extOf(from);
  if (ext === "") {
    // 目录改名：`目录` 无扩展名——按前缀映射各域打开中的文件
    for (const open of openNoteSessionFiles()) {
      const next = remapDirPrefix(open, from, file);
      if (next !== open) adoptRemoteNoteRelocate(open, next);
    }
    const canvas = useCanvasStore.getState();
    if (canvas.canvasFile !== null && canvas.canvasFile.startsWith(`${from}/`)) {
      const next = remapDirPrefix(canvas.canvasFile, from, file);
      followCanvasPath(canvas.canvasFile, next);
      if (canvasClean()) void useCanvasStore.getState().reloadFromDisk();
    }
    const table = useTableStore.getState();
    if (table.tableFile !== null && table.tableFile.startsWith(`${from}/`)) {
      const next = remapDirPrefix(table.tableFile, from, file);
      followTablePath(table.tableFile, next);
      if (tableClean()) void useTableStore.getState().reloadFromDisk();
    }
    return;
  }
  if (ext === "md") {
    for (const open of openNoteSessionFiles()) {
      if (open === from) adoptRemoteNoteRelocate(from, file);
    }
    return;
  }
  if (ext === "atlx") {
    if (useCanvasStore.getState().canvasFile === from) {
      followCanvasPath(from, file);
      if (canvasClean()) void useCanvasStore.getState().reloadFromDisk();
    }
    return;
  }
  if (ext === "atb") {
    if (useTableStore.getState().tableFile === from) {
      followTablePath(from, file);
      if (tableClean()) void useTableStore.getState().reloadFromDisk();
    }
    // 表格节点按 file 引用 .atb：引用被迁移方改写，打开中的画布重读对齐
    const canvas = useCanvasStore.getState();
    if (canvas.canvasFile !== null && canvasClean()) {
      void useCanvasStore.getState().reloadFromDisk();
    }
    return;
  }
  // 附件改名：media/text 节点引用被迁移方改写，打开中的画布重读对齐
  const canvas = useCanvasStore.getState();
  if (canvas.canvasFile !== null && canvasClean()) {
    const referenced =
      canvas.findTextNoteByFile(from) !== null || canvas.findMediaNoteByFile(from) !== null;
    if (referenced) void useCanvasStore.getState().reloadFromDisk();
  }
}

/** 本地仓库根的宽松比较键：分隔符统一为 `/`、大小写不敏感（Rust PathBuf 与前端身份串的书写差异）。 */
function rootKey(root: string): string {
  return root.replace(/\\/g, "/").toLowerCase();
}

/**
 * 分发一条广播（订阅回调与测试共用）。收敛一律复用域内机制，本函数只做「状态如何跟上」的决策；
 * 补丁合并在 Rust 侧每次落盘前重读磁盘完成，陈旧窗口的增量补丁不会覆盖他窗已落盘内容。
 * 不广播删除：外部删除本就「在打开/重读时表现」，陈旧窗口的下次写盘重建文件与既有口径一致。
 */
export function handleVaultContentChanged(payload: VaultContentChangedPayload): void {
  const identity = getActiveVaultIdentity();
  // 非本地仓库（空间真源在服务端）或仓库根不符（切仓库在途的迟到广播）：不分发
  if (identity?.kind !== "local" || rootKey(identity.root) !== rootKey(payload.root)) return;
  for (const change of payload.changes) {
    if (change.kind === "write") {
      const { file, from } = change;
      switch (extOf(file)) {
        case "atlx":
          canvasWritten(file, from);
          break;
        case "atb":
          tableWritten(file, from);
          break;
        case "md":
          noteWritten(file);
          break;
        default:
          // 附件字节变化：打开中的画布对引用节点补读
          if (useCanvasStore.getState().canvasFile !== null) {
            void useCanvasStore.getState().refreshMediaContent(file);
          }
      }
    } else {
      renamed(change.from, change.file);
    }
  }
}

/**
 * 注册跨窗口写盘订阅（窗口级接线，App 根组件挂载时调用，返回撤销函数）。
 * 主窗口/撕裂窗口/移动端各自挂一份；本窗口自己的写盘（origin = 本窗口）跳过——
 * 域内收敛已由既有信号覆盖。
 * 不在 store 模块的求值链上注册：本模块引入的编辑会话等 store 依赖完整求值顺序，
 * 过早求值会在模块环上拿到未初始化的导出（见 App 挂载时机的注释）。
 */
export function subscribeCrossWindowWrites(): () => void {
  let unlistened = false;
  const promise = listen<VaultContentChangedPayload>("vault-content-changed", (e) => {
    if (e.payload.origin === getCurrentWindow().label) return;
    handleVaultContentChanged(e.payload);
  });
  void promise.catch((e: unknown) => console.error("订阅跨窗口内容变更失败", e));
  return () => {
    if (unlistened) return;
    unlistened = true;
    void promise.then((un) => un());
  };
}
