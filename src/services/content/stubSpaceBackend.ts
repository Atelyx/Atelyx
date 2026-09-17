/**
 * 空间形态 stub 后端：内存文件树实现内容面契约（仅测试消费，不进生产 bundle）。
 *
 * 表达空间后端的最低语义：真源 = 内存树（无磁盘）、读按需取树、写后即真、
 * 写自动建父目录、失败如实抛出、删除不存在即报错。未实现的方法抛错——
 * 压测只承诺笔记域读写链，不模拟链接改写等重扫描行为。
 */
import { READ_WINDOW_DEFAULT_LINES } from "@/constants/tools";
import type { ContentBackend, VaultIdentity } from "./contract";

export function createSpaceStubBackend() {
  /** 内容文件树（相对路径 → 正文；目录由路径隐式表达，空目录不存在）。 */
  const files = new Map<string, string>();
  const writeOrder: string[] = [];
  const failContents = new Set<string>();
  const state = { reads: 0, writeDelayMs: 0 };

  async function write(file: string, content: string): Promise<void> {
    if (state.writeDelayMs > 0) await new Promise((r) => setTimeout(r, state.writeDelayMs));
    if (failContents.has(content)) throw new Error("后端写失败（stub 注入）");
    files.set(file, content);
    writeOrder.push(file);
  }

  function readText(file: string): string {
    const content = files.get(file);
    if (content === undefined) throw new Error(`文件不存在：${file}`);
    return content;
  }

  const backend: ContentBackend = {
    listTree: () => Promise.reject(noImpl("listTree")),
    listDir: () => Promise.reject(noImpl("listDir")),

    readFile: (file) => {
      state.reads += 1;
      return Promise.resolve(readText(file));
    },
    readFileWindow: (file, opts) => {
      state.reads += 1;
      const lines = readText(file).split("\n");
      const offset = Math.max(1, opts?.offset ?? 1);
      const limit = opts?.limit ?? READ_WINDOW_DEFAULT_LINES;
      const window = lines.slice(offset - 1, offset - 1 + limit);
      return Promise.resolve({
        lines: window.map((text, i) => ({ number: offset + i, text })),
        totalLines: lines.length,
        truncated: offset - 1 + limit < lines.length,
      });
    },
    readNote: (file) => {
      state.reads += 1;
      return Promise.resolve(readText(file));
    },
    readCanvas: () => Promise.reject(noImpl("readCanvas")),
    readTable: () => Promise.reject(noImpl("readTable")),
    listCanvases: () => Promise.reject(noImpl("listCanvases")),
    readAttachmentDataUrl: () => Promise.reject(noImpl("readAttachmentDataUrl")),

    writeFile: (file, content) => write(file, content),
    writeNote: (file, content) => write(file, content),
    writeCanvas: () => Promise.reject(noImpl("writeCanvas")),
    writeTable: () => Promise.reject(noImpl("writeTable")),
    createCanvas: () => Promise.reject(noImpl("createCanvas")),
    createTable: () => Promise.reject(noImpl("createTable")),

    patchCanvas: () => Promise.reject(noImpl("patchCanvas")),
    patchTable: () => Promise.reject(noImpl("patchTable")),

    renameNote: () => Promise.reject(noImpl("renameNote")),
    renameCanvas: () => Promise.reject(noImpl("renameCanvas")),
    moveCanvas: () => Promise.reject(noImpl("moveCanvas")),
    renameTable: () => Promise.reject(noImpl("renameTable")),
    moveTable: () => Promise.reject(noImpl("moveTable")),
    renameAttachment: () => Promise.reject(noImpl("renameAttachment")),
    renameFolder: () => Promise.reject(noImpl("renameFolder")),
    deleteNote: (file) => {
      if (!files.delete(file)) return Promise.reject(new Error(`文件不存在：${file}`));
      return Promise.resolve();
    },
    deleteAttachment: (file) => {
      if (!files.delete(file)) return Promise.reject(new Error(`文件不存在：${file}`));
      return Promise.resolve();
    },
    deleteCanvas: () => Promise.reject(noImpl("deleteCanvas")),
    deleteTable: () => Promise.reject(noImpl("deleteTable")),
    deleteFolder: () => Promise.reject(noImpl("deleteFolder")),
    createFolder: () => Promise.reject(noImpl("createFolder")),
    copyFile: () => Promise.reject(noImpl("copyFile")),
    copyFolder: () => Promise.reject(noImpl("copyFolder")),
    rebuildLinks: () => Promise.reject(noImpl("rebuildLinks")),

    remapSideloads: () => Promise.reject(noImpl("remapSideloads")),
    remapSideloadsByDir: () => Promise.reject(noImpl("remapSideloadsByDir")),

    writeTempAttachment: () => Promise.reject(noImpl("writeTempAttachment")),
    importAttachment: () => Promise.reject(noImpl("importAttachment")),
    importTableImage: () => Promise.reject(noImpl("importTableImage")),
    cleanupCanvasTempAttachments: () => Promise.reject(noImpl("cleanupCanvasTempAttachments")),
    cleanupTableAttachments: () => Promise.reject(noImpl("cleanupTableAttachments")),

    scanBacklinks: () => Promise.reject(noImpl("scanBacklinks")),
    scanTags: () => Promise.reject(noImpl("scanTags")),
    glob: () => Promise.reject(noImpl("glob")),
    grep: () => Promise.reject(noImpl("grep")),
    repoHistoryAggregate: () => Promise.reject(noImpl("repoHistoryAggregate")),
  };

  const identity: VaultIdentity = {
    kind: "space",
    serverUrl: "stub://space",
    spaceId: "stub-space",
  };

  return {
    identity,
    backend,
    files,
    writeOrder,
    failContents,
    /** 直接写入内存树（预置内容/测试助手；语义同 writeNote）。 */
    write: (file: string, content: string) => write(file, content),
    get reads() {
      return state.reads;
    },
    get writeDelayMs() {
      return state.writeDelayMs;
    },
    set writeDelayMs(v: number) {
      state.writeDelayMs = v;
    },
  };
}

function noImpl(method: string): Error {
  return new Error(`stub 空间后端未实现方法：${method}`);
}
