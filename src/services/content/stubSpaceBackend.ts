/**
 * 空间形态 stub 后端：内存文件树实现内容面契约（仅测试消费，不进生产 bundle）。
 *
 * 表达空间后端的最低语义：真源 = 内存树（无磁盘）、读按需取树、写后即真、
 * 写自动建父目录、失败如实抛出、删除不存在即报错。未实现的方法抛错——
 * 压测只承诺笔记域读写链，不模拟链接改写等重扫描行为。
 *
 * 画布/表格额外表达**按稳定 id 的增量补丁**（upsert 覆盖/removed 幂等 + 改名漂移），
 * 与空补丁/缺失文件语义一致；写盘延迟可注入（`writeDelayMs`），用于构造「写盘在途」的时序。
 */
import { READ_WINDOW_DEFAULT_LINES } from "@/constants/tools";
import { CANVAS_SCHEMA } from "@/constants/canvas";
import { TABLE_SCHEMA } from "@/constants/table";
import { parentDir, sanitizeFilename } from "@/utils/filename";
import type { CanvasFile, TableFile } from "@/types";
import type { ContentBackend, VaultIdentity } from "./contract";

export function createSpaceStubBackend() {
  /** 内容文件树（相对路径 → 正文；目录由路径隐式表达，空目录不存在）。 */
  const files = new Map<string, string>();
  const writeOrder: string[] = [];
  const failContents = new Set<string>();
  const state = { reads: 0, writeDelayMs: 0 };
  /** 实体写盘记录（画布/表格，按请求顺序）：测试据此断言写盘次序与串行化。 */
  const entityWrites: { kind: "canvas" | "table"; file: string }[] = [];

  /** 落盘（标题改名时旧路径删除），返回落盘戳（毫秒秒级，仅供契约形状）。 */
  function commit(file: string, nextFile: string, content: string): number {
    files.set(nextFile, content);
    writeOrder.push(nextFile);
    if (nextFile !== file) files.delete(file);
    return Math.floor(Date.now() / 1000);
  }

  /** 标题变更 = 同目录按标题改文件名（与本地命令/服务端补丁同语义）。 */
  function siblingEntityPath(file: string, title: string, ext: "atlx" | "atb"): string {
    const dir = parentDir(file);
    const name = `${sanitizeFilename(title)}.${ext}`;
    return dir ? `${dir}/${name}` : name;
  }

  /** 文件缺失按**字符串**抛出：store 的回退分支按 `includes("文件不存在（已从磁盘删除）")`
   *  判定，本地命令与空间后端镜像的都是字符串。 */
  function missingError(kind: "画布" | "表格"): string {
    return `${kind}文件不存在（已从磁盘删除）`;
  }

  function readEntity<T>(file: string, kind: "画布" | "表格"): T {
    const text = files.get(file);
    if (text === undefined) throw new Error(`${kind}文件不存在：${file}`);
    return JSON.parse(text) as T;
  }

  /** 按稳定 id 合并（removed 幂等；upsert 覆盖同 id 或追加）。 */
  function upsertById<T extends { id: string }>(list: T[], upserts: T[], removedIds: string[]): T[] {
    const removed = new Set(removedIds);
    const next = list.filter((x) => !removed.has(x.id));
    for (const item of upserts) {
      const i = next.findIndex((x) => x.id === item.id);
      if (i >= 0) next[i] = item;
      else next.push(item);
    }
    return next;
  }

  /** 按 id 全序重排：order 未出现的实体（并发新增）保持相对顺序置尾。 */
  function reorderById<T extends { id: string }>(list: T[], order?: string[]): T[] {
    if (!order) return list;
    const byId = new Map(list.map((x) => [x.id, x]));
    const ordered: T[] = [];
    for (const id of order) {
      const item = byId.get(id);
      if (item) {
        ordered.push(item);
        byId.delete(id);
      }
    }
    return [...ordered, ...byId.values()];
  }

  /** 注入的写盘延迟：用于构造「写盘在途 + 用户继续编辑」的时序。 */
  function delayWrite(): Promise<void> {
    return state.writeDelayMs > 0
      ? new Promise((r) => setTimeout(r, state.writeDelayMs))
      : Promise.resolve();
  }

  async function write(file: string, content: string): Promise<void> {
    await delayWrite();
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
    readCanvas: (file) => {
      const canvas = readEntity<CanvasFile>(file, "画布");
      if (canvas.schema !== CANVAS_SCHEMA) throw new Error(`画布 schema 不匹配：${file}`);
      return Promise.resolve(canvas);
    },
    readTable: (file) => {
      const table = readEntity<TableFile>(file, "表格");
      if (table.schema !== TABLE_SCHEMA) throw new Error(`表格 schema 不匹配：${file}`);
      return Promise.resolve(table);
    },
    listCanvases: () => Promise.reject(noImpl("listCanvases")),
    readAttachmentDataUrl: () => Promise.reject(noImpl("readAttachmentDataUrl")),

    writeFile: (file, content) => write(file, content),
    writeNote: (file, content) => write(file, content),
    async writeCanvas(canvas, file) {
      entityWrites.push({ kind: "canvas", file });
      await delayWrite();
      return commit(file, file, JSON.stringify(canvas));
    },
    async writeTable(table, file) {
      entityWrites.push({ kind: "table", file });
      await delayWrite();
      return commit(file, file, JSON.stringify(table));
    },
    createCanvas: () => Promise.reject(noImpl("createCanvas")),
    createTable: () => Promise.reject(noImpl("createTable")),

    async patchCanvas(patch, file) {
      entityWrites.push({ kind: "canvas", file });
      await delayWrite();
      if (!files.has(file)) throw missingError("画布");
      const canvas = readEntity<CanvasFile>(file, "画布");
      // 防串文件守卫：补丁属于另一画布 → 拒绝
      if (patch.id !== canvas.id) throw new Error("画布身份不匹配，已中止保存");
      if (patch.title !== undefined) canvas.title = patch.title;
      canvas.nodes = upsertById(canvas.nodes, patch.upsertNodes, patch.removedNodeIds);
      canvas.edges = upsertById(canvas.edges, patch.upsertEdges, patch.removedEdgeIds);
      const nextFile = siblingEntityPath(file, canvas.title, "atlx");
      return { updatedAt: commit(file, nextFile, JSON.stringify(canvas)), file: nextFile };
    },
    async patchTable(patch, file) {
      entityWrites.push({ kind: "table", file });
      await delayWrite();
      if (!files.has(file)) throw missingError("表格");
      const table = readEntity<TableFile>(file, "表格");
      if (patch.id !== table.id) throw new Error("表格身份不匹配，已中止保存");
      if (patch.title !== undefined) table.title = patch.title;
      table.fields = reorderById(
        upsertById(table.fields, patch.upsertFields, patch.removedFieldIds),
        patch.fieldOrder,
      );
      table.rows = reorderById(
        upsertById(table.rows, patch.upsertRows, patch.removedRowIds),
        patch.rowOrder,
      );
      const nextFile = siblingEntityPath(file, table.title, "atb");
      return { updatedAt: commit(file, nextFile, JSON.stringify(table)), file: nextFile };
    },

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
    listDatedNotes: () => Promise.reject(noImpl("listDatedNotes")),
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
    /** 预置实体（画布/表格）内容：模拟「空间里已有这个文件」。 */
    seed: (file: string, content: string) => {
      files.set(file, content);
    },
    /** 实体写盘记录（画布/表格，按请求顺序）：断言写盘次数与并发/串行。 */
    entityWrites,
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
