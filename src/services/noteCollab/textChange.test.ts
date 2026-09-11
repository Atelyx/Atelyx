/**
 * 笔记正文差量与三方合并纯函数测试（services/noteCollab/textChange.ts）。
 * 覆盖：hunk 定位正确性（按 hunk 重放必得目标文本）、多 hunk 精度、超限降级不丢内容、
 * 三方合并在非重叠区间保留双方、重叠区间按指定一侧取值且结果确定。
 */
import { describe, it, expect } from "vitest";
import { diffHunks, merge3, MAX_DIFF_TOKENS, type TextHunk } from "./textChange";

/** 按 hunk 重放：结果必须逐字符等于目标文本。 */
function applyHunks(base: string, hunks: TextHunk[]): string {
  let out = "";
  let pos = 0;
  for (const h of hunks) {
    out += base.slice(pos, h.at) + h.insert;
    pos = h.at + h.remove;
  }
  return out + base.slice(pos);
}

describe("diffHunks", () => {
  it("同文本 → 空表", () => {
    expect(diffHunks("abc", "abc")).toEqual([]);
  });

  it("各类改写都能按 hunk 重放还原目标文本", () => {
    const cases: Array<[string, string]> = [
      ["", "abc"],
      ["abc", ""],
      ["abc", "abc"],
      ["abc", "abcdef"],
      ["abcdef", "abc"],
      ["hello world", "hello brave world"],
      ["line1\nline2\nline3\n", "line1\nLINE-TWO\nline3\n"],
      ["a\nb\nc\nd\n", "a\nc\nd\n"],
      ["a\nc\nd\n", "a\nb\nc\nd\n"],
      ["头部\n中段\n尾部\n", "开头改动\n中段\n末尾改动\n"],
      ["# 标题\r\n\r\n正文\r\n", "# 标题\r\n\r\n新正文\r\n"],
      ["emoji😀保留", "emoji😀保留!"],
      ["没有换行的单行文本", "没有换行的单行文本（追加）"],
      ["a\n\n\nb\n", "a\nb\n"],
    ];
    for (const [base, next] of cases) {
      const hunks = diffHunks(base, next);
      expect(applyHunks(base, hunks), `${JSON.stringify(base)} → ${JSON.stringify(next)}`)
        .toBe(next);
    }
  });

  it("多处不相邻改写产生多个 hunk（不做整段替换）", () => {
    const base = "前言\n第一段\n中段\n第二段\n结束\n";
    const next = "前言改了\n第一段\n中段\n第二段改了\n结束\n";
    const hunks = diffHunks(base, next);
    expect(hunks.length).toBe(2);
    expect(applyHunks(base, hunks)).toBe(next);
  });

  it("在中间插入一行只产生纯插入 hunk（remove = 0）", () => {
    const base = "a\nb\nc\n";
    const next = "a\nb\n插入\nc\n";
    const hunks = diffHunks(base, next);
    expect(hunks).toHaveLength(1);
    expect(hunks[0].remove).toBe(0);
    expect(hunks[0].insert).toBe("插入\n");
    expect(applyHunks(base, hunks)).toBe(next);
  });

  it("超限降级为单 hunk 仍能还原目标文本", () => {
    const baseLines: string[] = [];
    const nextLines: string[] = [];
    const total = MAX_DIFF_TOKENS + 50;
    for (let i = 0; i < total; i++) {
      baseLines.push(`base-${i}\n`);
      nextLines.push(`next-${i}\n`);
    }
    const base = baseLines.join("");
    const next = nextLines.join("");
    const hunks = diffHunks(base, next);
    expect(hunks).toHaveLength(1);
    expect(applyHunks(base, hunks)).toBe(next);
  });
});

describe("merge3", () => {
  it("同文本 / 一侧未变时取另一侧", () => {
    expect(merge3("base", "same", "same", "theirs")).toBe("same");
    expect(merge3("base", "base", "theirs", "theirs")).toBe("theirs");
    expect(merge3("base", "ours", "base", "theirs")).toBe("ours");
  });

  it("非重叠区间保留双方改动", () => {
    const base = "第一段\n第二段\n第三段\n";
    const ours = "第一段改了\n第二段\n第三段\n";
    const theirs = "第一段\n第二段\n第三段改了\n";
    const merged = merge3(base, ours, theirs, "theirs");
    expect(merged).toBe("第一段改了\n第二段\n第三段改了\n");
  });

  it("一侧纯插入 + 另一侧别处改写：两者都在", () => {
    const base = "a\nb\nc\n";
    const ours = "a\n新增\nb\nc\n";
    const theirs = "a\nb\nC改\n";
    const merged = merge3(base, ours, theirs, "theirs");
    expect(merged).toBe("a\n新增\nb\nC改\n");
  });

  it("同区间改写按指定一侧取值", () => {
    const base = "整段内容\n";
    const ours = "本地版本\n";
    const theirs = "对端版本\n";
    expect(merge3(base, ours, theirs, "theirs")).toBe("对端版本\n");
    expect(merge3(base, ours, theirs, "ours")).toBe("本地版本\n");
  });

  it("同点插入视为同区间冲突，按指定一侧取值（结果确定）", () => {
    const base = "a\n";
    const ours = "a\n本地插入\n";
    const theirs = "a\n对端插入\n";
    expect(merge3(base, ours, theirs, "theirs")).toBe("a\n对端插入\n");
    expect(merge3(base, ours, theirs, "ours")).toBe("a\n本地插入\n");
  });

  it("插入锚点落在对方区间端点：插入内容保留（两侧改动都在）", () => {
    const base = "a\ndel1\ndel2\nz\n";
    const ours = "a\ndel1\ndel2\n插在删除段之后\nz\n";
    const theirs = "a\nz\n";
    // 对端删掉 del1/del2，本端在删除段末端插入：删除与插入都保留
    expect(merge3(base, ours, theirs, "theirs")).toBe("a\n插在删除段之后\nz\n");
    expect(merge3(base, ours, theirs, "ours")).toBe("a\n插在删除段之后\nz\n");
  });

  it("插入锚点严格落在对方区间内部：按冲突取一侧（该插入未被对端碰到也不保留）", () => {
    const base = "a\nb\nc\n";
    const ours = "a\nb\n插入\nc\n";
    const theirs = "a\nB改\nC改\n";
    // ours 的插入锚点（b 之后）严格落在 theirs 的替换区间 [b\nc\n] 内部 → 整块取指定一侧
    const byTheirs = merge3(base, ours, theirs, "theirs");
    const byOurs = merge3(base, ours, theirs, "ours");
    expect(byTheirs).toBe("a\nB改\nC改\n");
    expect(byOurs).toBe("a\nb\n插入\nc\n");
    expect(byTheirs).not.toBe(byOurs);
  });

  it("替换区间部分重叠：按冲突整块取一侧（hunk 替换文本不可拆分）", () => {
    const base = "L1\nL2\nL3\nL4\n";
    const ours = "L1\nX\nL4\n";
    const theirs = "L1\nL2\nY\n";
    expect(merge3(base, ours, theirs, "theirs")).toBe("L1\nL2\nY\n");
    expect(merge3(base, ours, theirs, "ours")).toBe("L1\nX\nL4\n");
  });

  it("插入点远离对方改动区间时两侧都保留", () => {
    const base = "首段\n中段\ndel1\ndel2\n末段\n";
    const ours = "新增在头部\n首段\n中段\ndel1\ndel2\n末段\n";
    const theirs = "首段\n中段\n末段\n";
    const merged = merge3(base, ours, theirs, "theirs");
    expect(merged).toBe("新增在头部\n首段\n中段\n末段\n");
  });

  it("同区间冲突结果与入参顺序无关（换侧调用的结果一致）", () => {
    const base = "整段内容\n";
    const ours = "本地版本\n";
    const theirs = "对端版本\n";
    expect(merge3(base, ours, theirs, "theirs")).toBe(merge3(base, theirs, ours, "ours"));
  });

  it("CRLF 与空文本边界", () => {
    expect(merge3("", "", "内容\r\n", "theirs")).toBe("内容\r\n");
    expect(merge3("a\r\nb\r\n", "a\r\nB\r\n", "a\r\nb\r\n", "theirs")).toBe("a\r\nB\r\n");
  });

  it("随机化：两侧各替换不同行（hunk 互不重叠）时，双方标记都必须保留且不重复", () => {
    const marker = (side: string, i: number) => `${side}${i}`;
    for (let round = 0; round < 20000; round++) {
      const lines = ["L0", "L1", "L2", "L3", "L4", "L5", "L6", "L7"];
      const base = `${lines.join("\n")}\n`;
      // 互不相同的两行分别改写，构造「两侧改动不重叠」的输入
      const i = Math.floor(Math.random() * lines.length);
      let j = Math.floor(Math.random() * lines.length);
      if (j === i) j = (j + 1) % lines.length;
      const ourMarker = marker("OURS", round);
      const theirMarker = marker("THEIRS", round);
      const oursLines = [...lines];
      const theirsLines = [...lines];
      oursLines[i] = ourMarker;
      theirsLines[j] = theirMarker;
      const ours = `${oursLines.join("\n")}\n`;
      const theirs = `${theirsLines.join("\n")}\n`;

      const byTheirs = merge3(base, ours, theirs, "theirs");
      const byOurs = merge3(base, ours, theirs, "ours");
      for (const merged of [byTheirs, byOurs]) {
        expect(countOf(merged, ourMarker)).toBe(1);
        expect(countOf(merged, theirMarker)).toBe(1);
      }
      // 换侧调用结果一致（结果只由入参决定）
      expect(merge3(base, theirs, ours, "ours")).toBe(byTheirs);
    }
  });
});

const countOf = (text: string, needle: string) => text.split(needle).length - 1;
