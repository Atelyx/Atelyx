/**
 * 笔记写盘链测试（stores/noteStore.ts 的按文件串行写盘队列）。
 * 只覆盖不依赖真实仓库 I/O 的语义：同文件并发保存**串行完成**且最终内容 = 最后一次调用、
 * 不同文件可并行、前序写盘失败不阻断本序。
 * 写盘经 Tauri 命令 `write_note`；mock 内人为制造在途窗口，否则调用顺序天然等于完成顺序，测不出串行性。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  /** 成功完成的写盘（按完成顺序）。 */
  writes: [] as { file: string; content: string }[],
  failFor: [] as string[],
  /** 单文件在途数与其峰值（队列生效 ⇒ 峰值恒为 1）。 */
  inFlight: new Map<string, number>(),
  maxInFlight: new Map<string, number>(),
  /** 全局在途数与峰值（用于证明跨文件不互相阻塞）。 */
  totalInFlight: 0,
  maxTotalInFlight: 0,
  /** 写盘在途窗口（毫秒）；越大越容易暴露并发。 */
  delayMs: 0,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args: unknown) => {
    if (cmd !== "write_note") return "";
    const { file, content } = args as { file: string; content: string };
    h.inFlight.set(file, (h.inFlight.get(file) ?? 0) + 1);
    h.maxInFlight.set(file, Math.max(h.maxInFlight.get(file) ?? 0, h.inFlight.get(file)!));
    h.totalInFlight += 1;
    h.maxTotalInFlight = Math.max(h.maxTotalInFlight, h.totalInFlight);
    try {
      if (h.delayMs > 0) await new Promise((r) => setTimeout(r, h.delayMs));
      if (h.failFor.includes(content)) throw new Error("write failed");
      h.writes.push({ file, content });
      return "";
    } finally {
      h.inFlight.set(file, h.inFlight.get(file)! - 1);
      h.totalInFlight -= 1;
    }
  },
}));

type NoteStore = typeof import("./noteStore");

let noteStore: NoteStore;

beforeEach(async () => {
  vi.resetModules();
  h.writes = [];
  h.failFor = [];
  h.inFlight = new Map();
  h.maxInFlight = new Map();
  h.totalInFlight = 0;
  h.maxTotalInFlight = 0;
  h.delayMs = 0;
  // 先起 noteSessionStore 再取 noteStore：两者与 pluginStore/builtins 成环，
  // 反向导入顺序会在环上取到尚未初始化的 useNoteStore（同 noteSessionStore.test.ts）
  await import("./noteSessionStore");
  noteStore = await import("./noteStore");
});

describe("按文件串行写盘队列", () => {
  it("同文件并发保存串行完成（在途峰值 1），最终内容 = 最后一次调用", async () => {
    h.delayMs = 5;
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    await Promise.all([
      saveNoteContent("a.md", "第一版"),
      saveNoteContent("a.md", "第二版"),
      saveNoteContent("a.md", "第三版"),
    ]);
    expect(h.maxInFlight.get("a.md")).toBe(1);
    expect(h.writes.map((w) => w.content)).toEqual(["第一版", "第二版", "第三版"]);
  });

  it("不同文件并行（全局在途峰值 ≥ 2）而同文件仍串行", async () => {
    h.delayMs = 10;
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    await Promise.all([
      saveNoteContent("a.md", "A1"),
      saveNoteContent("b.md", "B1"),
      saveNoteContent("a.md", "A2"),
    ]);
    expect(h.maxTotalInFlight).toBeGreaterThanOrEqual(2);
    expect(h.maxInFlight.get("a.md")).toBe(1);
    expect(h.maxInFlight.get("b.md")).toBe(1);
    expect(h.writes.filter((w) => w.file === "a.md").map((w) => w.content)).toEqual(["A1", "A2"]);
    expect(h.writes.filter((w) => w.file === "b.md").map((w) => w.content)).toEqual(["B1"]);
  });

  it("前序写盘失败不阻断本序，后序仍落盘", async () => {
    h.delayMs = 5;
    h.failFor = ["坏版本"];
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    const failed = saveNoteContent("a.md", "坏版本");
    const next = saveNoteContent("a.md", "好版本");
    await expect(failed).rejects.toThrow("write failed");
    await next;
    expect(h.writes.map((w) => w.content)).toEqual(["好版本"]);
  });
});
