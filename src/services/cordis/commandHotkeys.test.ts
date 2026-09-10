/**
 * 命令快捷键匹配测试（services/cordis/commandHotkeys 的 matchesShortcut）。
 */
import { describe, expect, it } from "vitest";
import { matchesShortcut } from "./commandHotkeys";

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
});
