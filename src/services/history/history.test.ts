/**
 * 通用历史服务契约测试。
 *
 * - 侧文件路径：note 保持旧路径向后兼容，canvas/table 分目录摊平。
 * - 版本管理：追加 seq、同内容去重、同作者连续编辑 60s 合并（滑动更新）。
 * - 作者门控：Agent 写入（`authorOverride`）不与用户版本串版；Agent 连续写入才合并。
 * - `recordAgentFileWrite`：跳过 `.atelyx/` 隐藏目录、按扩展名定 kind、content 缺省读回。
 */
import { beforeEach, describe, it, expect, vi } from "vitest";

// 内存文件 I/O mock（历史侧文件读写走 services/vault/aiFiles；Tauri invoke 在测试环境不可用）
const { memory } = vi.hoisted(() => ({
  memory: new Map<string, string>(),
}));
vi.mock("@/services/vault/aiFiles", () => ({
  readVaultFile: (file: string) => {
    const v = memory.get(file);
    return v === undefined
      ? Promise.reject(new Error(`not found: ${file}`))
      : Promise.resolve(v);
  },
  writeVaultFile: (file: string, content: string) => {
    memory.set(file, content);
    return Promise.resolve();
  },
  deleteVaultFile: (file: string) => {
    memory.delete(file);
    return Promise.resolve();
  },
}));

import {
  historyPathFor,
  setHistoryAuthor,
  loadHistory,
  recordHistoryVersion,
  recordAgentFileWrite,
  migrateHistoryFile,
  AGENT_AUTHOR,
  versionContentAt,
} from "./index";

const F = "notes/a.md";
const CANVAS = "c/方案.atlx";
const TABLE = "t/分镜.atb";

beforeEach(() => {
  memory.clear();
  setHistoryAuthor({ id: "dev-1", name: "张三", device: "dev-1" });
});

describe("historyPathFor", () => {
  it("纯 ASCII 路径：仅转义路径分隔符", () => {
    expect(historyPathFor("note", F)).toBe(".atelyx/history/notes%2Fa.md.json");
  });
  it("canvas/table 按 kind 分目录，CJK 保留原样", () => {
    expect(historyPathFor("canvas", CANVAS)).toBe(".atelyx/history/canvas/c%2F方案.atlx.json");
    expect(historyPathFor("table", TABLE)).toBe(".atelyx/history/table/t%2F分镜.atb.json");
  });
  it("长中文路径的侧文件相对路径有界（防全量编码膨胀超 NAS 路径上限）", () => {
    // encodeURIComponent 会把每个 CJK 字符膨胀成 9 字符；最小编码只转义非法字符，
    // 侧文件路径长度 ≈ 笔记自身路径 + 固定前缀/后缀——笔记可访问则侧文件必可访问
    const long = "动画剧本/没做完的剧本/" + "很长的中文标题".repeat(10) + ".md";
    const p = historyPathFor("note", long);
    const legacy = `.atelyx/history/${encodeURIComponent(long)}.json`;
    expect(p.length).toBeLessThan(legacy.length);
    expect(p.length).toBeLessThan(220);
    expect(p).toBe(`.atelyx/history/动画剧本%2F没做完的剧本%2F${"很长的中文标题".repeat(10)}.md.json`);
  });
});

describe("recordHistoryVersion / loadHistory", () => {
  it("追加版本并递增 seq、作者为当前用户", async () => {
    await recordHistoryVersion("note", F, { content: "v1", action: "edit" });
    await recordHistoryVersion("note", F, { content: "v2", action: "edit" });
    const vs = await loadHistory("note", F);
    expect(vs.map((v) => v.seq)).toEqual([1, 2]);
    expect(vs[1].author.name).toBe("张三");
  });

  it("内容 + 行为相同 no-op 去重", async () => {
    await recordHistoryVersion("note", F, { content: "v1", action: "edit" });
    await recordHistoryVersion("note", F, { content: "v1", action: "edit" });
    expect((await loadHistory("note", F)).length).toBe(1);
  });

  it("同作者连续编辑 60s 内合并为一版（滑动更新到最新）", async () => {
    await recordHistoryVersion("note", F, { content: "v1", action: "edit", coalesceEditMs: 60_000 });
    await recordHistoryVersion("note", F, { content: "v2", action: "edit", coalesceEditMs: 60_000 });
    const vs = await loadHistory("note", F);
    expect(vs.length).toBe(1);
    expect(vs[0].content).toBe("v2");
  });

  it("Agent 写入不与用户版本串版（作者不同不合并）", async () => {
    await recordHistoryVersion("note", F, { content: "user1", action: "edit", coalesceEditMs: 60_000 });
    await recordHistoryVersion("note", F, {
      content: "agent1",
      action: "edit",
      coalesceEditMs: 60_000,
      authorOverride: AGENT_AUTHOR,
    });
    const vs = await loadHistory("note", F);
    expect(vs.length).toBe(2);
    expect(vs[1].author.id).toBe("ai-agent");
  });

  it("Agent 连续写入合并、恢复用户编辑后再开新版本", async () => {
    await recordHistoryVersion("note", F, {
      content: "a1",
      action: "edit",
      coalesceEditMs: 60_000,
      authorOverride: AGENT_AUTHOR,
    });
    await recordHistoryVersion("note", F, {
      content: "a2",
      action: "edit",
      coalesceEditMs: 60_000,
      authorOverride: AGENT_AUTHOR,
    });
    expect((await loadHistory("note", F)).length).toBe(1);
    await recordHistoryVersion("note", F, { content: "user", action: "edit", coalesceEditMs: 60_000 });
    expect((await loadHistory("note", F)).length).toBe(2);
  });

  it("versionContentAt 按 seq 取快照", async () => {
    await recordHistoryVersion("canvas", CANVAS, { content: "{}", action: "edit" });
    const vs = await loadHistory("canvas", CANVAS);
    expect(versionContentAt(vs, 1)).toBe("{}");
    expect(versionContentAt(vs, 99)).toBeNull();
  });

  it("字节预算剪枝：超预算丢最旧保最新（防膨胀触及读上限被整份清空）", async () => {
    const big = "x".repeat(100);
    // 注入小预算（400B）：4 条 ~500B 超限 → 只留最新且落盘 ≤ 预算
    for (let i = 1; i <= 4; i++) {
      await recordHistoryVersion("note", F, {
        content: big + i,
        action: "edit",
        coalesceEditMs: 0,
        byteBudget: 400,
      });
    }
    const vs = await loadHistory("note", F);
    expect(vs.length).toBeLessThan(4);
    expect(vs[vs.length - 1].content).toBe(big + "4"); // 最新必留
    expect(vs[0].content).not.toBe(big + "1"); // 最旧被丢
    const side = memory.get(historyPathFor("note", F)) ?? "";
    expect(new TextEncoder().encode(side).length).toBeLessThanOrEqual(400);
  });

  it("coalesce 滑动更新同样受字节预算约束（单次增长不突破读上限）", async () => {
    // 预算 600B：3 条小版本入栈，最后一条经 coalesce 滑动更新撑大 → 总量超预算，
    // 写盘时剪掉最旧版本，最新内容保留且落盘 ≤ 预算
    await recordHistoryVersion("note", F, { content: "a", action: "edit", coalesceEditMs: 0, byteBudget: 600 });
    await recordHistoryVersion("note", F, { content: "b", action: "edit", coalesceEditMs: 0, byteBudget: 600 });
    await recordHistoryVersion("note", F, { content: "c", action: "edit", coalesceEditMs: 0, byteBudget: 600 });
    const big = "x".repeat(400);
    await recordHistoryVersion("note", F, { content: big, action: "edit", coalesceEditMs: 60_000, byteBudget: 600 });
    const vs = await loadHistory("note", F);
    expect(vs[vs.length - 1].content).toBe(big); // 最新（coalesce 更新）必留
    const side = memory.get(historyPathFor("note", F)) ?? "";
    expect(new TextEncoder().encode(side).length).toBeLessThanOrEqual(600);
  });

  it("存量旧编码侧文件：读取回退 + 记录时迁移到新编码名并删除旧文件", async () => {
    // CJK 路径新旧编码名不同（纯 ASCII 两者相同，无迁移意义）
    const f = "笔记/示例.md";
    const legacyPath = `.atelyx/history/${encodeURIComponent(f)}.json`;
    const newPath = historyPathFor("note", f);
    expect(legacyPath).not.toBe(newPath);
    memory.set(
      legacyPath,
      JSON.stringify({
        versions: [
          { seq: 1, ts: 1, author: { id: "dev-1", name: "张三", device: "dev-1" }, action: "edit", content: "old" },
        ],
      }),
    );
    // 读取回退：新名缺失 → 读到旧编码存量
    expect((await loadHistory("note", f)).map((v) => v.content)).toEqual(["old"]);
    // 记录新版本 → 迁移：新编码名含全部版本，旧文件删除
    await recordHistoryVersion("note", f, { content: "new", action: "edit" });
    expect(memory.has(legacyPath)).toBe(false);
    expect(memory.has(newPath)).toBe(true);
    expect((await loadHistory("note", f)).map((v) => v.content)).toEqual(["old", "new"]);
  });

  it("纯 ASCII 路径 migrate 不删除真实历史（新旧编码名相同，无旧文件可清）", async () => {
    // 回归：migrateHistoryFile 若无条件删 legacy 路径，ASCII 名（新旧名相同）会把
    // 真实历史侧文件当「旧文件」删掉——重命名/移动 ASCII 笔记即丢全部历史
    const ascii = "notes/a.md";
    const side = historyPathFor("note", ascii);
    expect(side).toBe(`.atelyx/history/${encodeURIComponent(ascii)}.json`); // 新旧名一致的判定前提
    memory.set(
      side,
      JSON.stringify({
        versions: [
          { seq: 1, ts: 1, author: { id: "dev-1", name: "张三", device: "dev-1" }, action: "edit", content: "v1" },
        ],
      }),
    );
    await migrateHistoryFile("note", ascii);
    expect(memory.has(side)).toBe(true); // 历史必须保留
    expect((await loadHistory("note", ascii)).map((v) => v.content)).toEqual(["v1"]);
  });

  it("表格单元格存档点（recordTableHistory 同款内存快照）可记录并读回", async () => {
    // 模拟 tableStore.recordTableHistory：以内存 fields/rows 构建 `.atb` 格式快照
    const snapshot = {
      schema: "atelyx-table/v1",
      id: "tbl-1",
      title: "分镜",
      fields: [{ id: "f1", name: "镜号", fieldType: "text" }],
      rows: [{ id: "r1", values: { f1: "01" } }],
      createdAt: 0,
      updatedAt: 1700000000,
    };
    await recordHistoryVersion("table", TABLE, {
      content: JSON.stringify(snapshot),
      action: "edit",
      coalesceEditMs: 60_000,
    });
    const vs = await loadHistory("table", TABLE);
    expect(vs.length).toBe(1);
    const parsed = JSON.parse(vs[0].content) as {
      schema: string;
      title: string;
      fields: unknown[];
      rows: unknown[];
    };
    expect(parsed.schema).toBe("atelyx-table/v1");
    expect(parsed.title).toBe("分镜");
    expect(Array.isArray(parsed.fields) && parsed.fields.length).toBe(1);
    expect(Array.isArray(parsed.rows) && parsed.rows.length).toBe(1);
  });
});

describe("recordAgentFileWrite", () => {
  it("跳过 .atelyx/ 隐藏目录（防历史自身写递归/噪声）", async () => {
    await recordAgentFileWrite(".atelyx/history/x.json", "x");
    expect(await loadHistory("note", ".atelyx/history/x.json")).toEqual([]);
  });

  it("按扩展名定 kind 并以 Agent 身份记录", async () => {
    await recordAgentFileWrite(CANVAS, '{"schema":"atelyx-canvas/v1"}');
    const vs = await loadHistory("canvas", CANVAS);
    expect(vs.length).toBe(1);
    expect(vs[0].author.id).toMatch(/^ai-agent/);
    expect(vs[0].action).toBe("edit");
  });

  it("content 缺省读回磁盘（edit_file 场景）", async () => {
    memory.set(TABLE, '{"schema":"atelyx-table/v1"}');
    await recordAgentFileWrite(TABLE);
    const vs = await loadHistory("table", TABLE);
    expect(vs.length).toBe(1);
    expect(vs[0].content).toBe('{"schema":"atelyx-table/v1"}');
  });
});
