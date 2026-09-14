/**
 * 命令快捷键测试（services/cordis/commandHotkeys）：matchesShortcut 修饰键精确匹配
 * 与 dispatchHotkey 的执行/异常隔离。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { dispatchHotkey, matchesShortcut } from "./commandHotkeys";

const ev = (partial: Record<string, unknown>) => partial as unknown as KeyboardEvent;

describe("matchesShortcut", () => {
  it("mod+k：Ctrl/Cmd + K 命中；无修饰不命中", () => {
    expect(matchesShortcut("mod+k", ev({ key: "k", ctrlKey: true }))).toBe(true);
    expect(matchesShortcut("mod+k", ev({ key: "k", metaKey: true }))).toBe(true);
    expect(matchesShortcut("mod+k", ev({ key: "k" }))).toBe(false);
    expect(matchesShortcut("mod+k", ev({ key: "j", ctrlKey: true }))).toBe(false);
  });

  it("shift+mod+p：大小写不敏感，需要 shift + mod", () => {
    expect(matchesShortcut("shift+mod+p", ev({ key: "P", ctrlKey: true, shiftKey: true }))).toBe(true);
    expect(matchesShortcut("shift+mod+p", ev({ key: "p", metaKey: true, shiftKey: true }))).toBe(true);
    expect(matchesShortcut("shift+mod+p", ev({ key: "P", ctrlKey: true }))).toBe(false); // 缺 shift
    expect(matchesShortcut("shift+mod+p", ev({ key: "P", shiftKey: true }))).toBe(false); // 缺 mod
  });

  it("纯按键（无修饰）匹配主键", () => {
    expect(matchesShortcut("f1", ev({ key: "F1" }))).toBe(true);
    expect(matchesShortcut("f1", ev({ key: "F2" }))).toBe(false);
  });

  it("额外修饰键不命中（修饰键集合精确匹配）", () => {
    // 注册 ctrl+k，按 Ctrl+Shift+K / Ctrl+Alt+K / Ctrl+Meta+K 都不算命中
    expect(matchesShortcut("ctrl+k", ev({ key: "k", ctrlKey: true, shiftKey: true }))).toBe(false);
    expect(matchesShortcut("ctrl+k", ev({ key: "k", ctrlKey: true, altKey: true }))).toBe(false);
    expect(matchesShortcut("ctrl+k", ev({ key: "k", ctrlKey: true, metaKey: true }))).toBe(false);
    // 注册 mod+k，多按的 alt/shift 同样不命中
    expect(matchesShortcut("mod+k", ev({ key: "k", ctrlKey: true, altKey: true }))).toBe(false);
    expect(matchesShortcut("mod+k", ev({ key: "K", metaKey: true, shiftKey: true }))).toBe(false);
  });

  it("ctrl 与 meta 不互相冒充（mod 之外精确到声明键）", () => {
    expect(matchesShortcut("ctrl+k", ev({ key: "k", metaKey: true }))).toBe(false);
    expect(matchesShortcut("meta+k", ev({ key: "k", ctrlKey: true }))).toBe(false);
    expect(matchesShortcut("alt+k", ev({ key: "k", altKey: true }))).toBe(true);
    expect(matchesShortcut("alt+k", ev({ key: "k", altKey: true, ctrlKey: true }))).toBe(false);
  });
});

describe("dispatchHotkey", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  const command = (over: { shortcut: string; run: () => unknown; id: string }) => ({
    pluginId: "com.test",
    id: over.id,
    label: "运行",
    shortcut: over.shortcut,
    run: over.run,
  });

  it("首条命中执行并 preventDefault，其余命令不再执行", () => {
    const runA = vi.fn();
    const runB = vi.fn();
    const preventDefault = vi.fn();
    const hit = dispatchHotkey(ev({ key: "k", ctrlKey: true, preventDefault }), [
      command({ id: "a", shortcut: "mod+j", run: runA }),
      command({ id: "b", shortcut: "ctrl+k", run: runA }),
      command({ id: "c", shortcut: "ctrl+k", run: runB }),
    ]);
    expect(hit).toBe(true);
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(runA).toHaveBeenCalledTimes(1);
    expect(runB).not.toHaveBeenCalled();
  });

  it("无命中不 preventDefault，返回 false", () => {
    const preventDefault = vi.fn();
    const hit = dispatchHotkey(ev({ key: "k", ctrlKey: true, preventDefault }), [
      command({ id: "a", shortcut: "ctrl+j", run: () => undefined }),
    ]);
    expect(hit).toBe(false);
    expect(preventDefault).not.toHaveBeenCalled();
  });

  it("同步抛错不冒出监听器（console.error 收敛），异步 rejection 同样收敛", async () => {
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const boom = new Error("同步炸");
    dispatchHotkey(ev({ key: "k", ctrlKey: true, preventDefault: () => {} }), [
      command({ id: "sync", shortcut: "ctrl+k", run: () => { throw boom; } }),
    ]);
    expect(errSpy).toHaveBeenCalledWith(boom);

    const rejection = new Error("异步炸");
    dispatchHotkey(ev({ key: "j", ctrlKey: true, preventDefault: () => {} }), [
      command({ id: "async", shortcut: "ctrl+j", run: () => Promise.reject(rejection) }),
    ]);
    await Promise.resolve();
    await Promise.resolve();
    expect(errSpy).toHaveBeenCalledWith(rejection);
  });
});
