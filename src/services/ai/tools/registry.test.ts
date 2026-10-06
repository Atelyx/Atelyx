/**
 * 工具注册表并行分发契约测试（services/ai/tools/registry）：连续 `parallelSafe` 调用在有界滚动池内并发执行、结果按模型调用顺序回填，在飞数不超 `maxParallel`。
 * 非 `parallelSafe`（读写）调用单独成段形成屏障（前段收敛前不启动）；单调用失败只降级该调用；中止后不再启动新调用并跳过结果回填。
 */
import { describe, expect, it } from "vitest";
import type { LlmToolCall, ToolDefinition, ToolExecContext } from "@/types";
import { defineTool } from "./defineTool";
import { createToolRegistry } from "./registry";

interface FakeToolOpts {
  parallelSafe?: boolean;
  execute?: (callId: string) => Promise<{ ok: boolean; summary: string }>;
}

function fakeTool(name: string, opts: FakeToolOpts = {}): ToolDefinition {
  return defineTool({
    name,
    description: name,
    parameters: {},
    validate: (args) =>
      args && typeof args === "object"
        ? (args as Record<string, unknown>)
        : {},
    summarize: () => name,
    execute: async (args) => {
      const callId = (args as { callId?: string })?.callId ?? name;
      return opts.execute ? opts.execute(callId) : { ok: true, summary: name };
    },
    ...(opts.parallelSafe ? { parallelSafe: true } : {}),
  });
}

const call = (name: string, id: string): LlmToolCall => ({
  id,
  name,
  arguments: JSON.stringify({ callId: id }),
});

const ctx = (signal?: AbortSignal): ToolExecContext => ({
  signal: signal ?? new AbortController().signal,
  capabilities: {},
});

function deferred<T = void>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

const tick = (ms = 10) => new Promise((r) => setTimeout(r, ms));

describe("registry.dispatch 并行执行", () => {
  it("并行安全段内并发执行（全部同时进行）", async () => {
    let active = 0;
    let maxActive = 0;
    const gate = deferred();
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async () => {
        active++;
        maxActive = Math.max(maxActive, active);
        await gate.promise;
        active--;
        return { ok: true, summary: "read" };
      },
    });
    const registry = createToolRegistry([tool]);
    const p = registry.dispatch(
      [call("read", "c1"), call("read", "c2"), call("read", "c3")],
      ctx(),
    );
    await tick();
    expect(maxActive).toBe(3);
    gate.resolve();
    const res = await p;
    expect(res.results.map((r) => r.id)).toEqual(["c1", "c2", "c3"]);
  });

  it("在飞数量不超过 maxParallel（有界滚动池）", async () => {
    let active = 0;
    let maxActive = 0;
    const gate = deferred();
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async () => {
        active++;
        maxActive = Math.max(maxActive, active);
        await gate.promise;
        active--;
        return { ok: true, summary: "read" };
      },
    });
    const registry = createToolRegistry([tool]);
    const p = registry.dispatch(
      [call("read", "c1"), call("read", "c2"), call("read", "c3")],
      ctx(),
      2,
    );
    await tick();
    expect(maxActive).toBe(2);
    gate.resolve();
    await p;
  });

  it("结果按模型调用顺序回填（并行段 + 屏障交错）", async () => {
    const log: string[] = [];
    const readTool = fakeTool("read", {
      parallelSafe: true,
      execute: async (callId) => {
        log.push(`${callId}:start`);
        await new Promise((r) => setTimeout(r, callId === "c1" ? 20 : 5));
        log.push(`${callId}:end`);
        return { ok: true, summary: callId };
      },
    });
    const writeTool = fakeTool("write", {
      execute: async (callId) => {
        log.push(`${callId}:start`);
        log.push(`${callId}:end`);
        return { ok: true, summary: callId };
      },
    });
    const registry = createToolRegistry([readTool, writeTool]);
    const res = await registry.dispatch(
      [call("read", "c1"), call("write", "c2"), call("read", "c3")],
      ctx(),
    );
    expect(res.results.map((r) => r.id)).toEqual(["c1", "c2", "c3"]);
    expect(
      res.messages.map((m) => (m as { toolCallId?: string }).toolCallId),
    ).toEqual(["c1", "c2", "c3"]);
  });

  it("非并行安全（读写）工具形成屏障（前段收敛后才启动）", async () => {
    const log: string[] = [];
    const readTool = fakeTool("read", {
      parallelSafe: true,
      execute: async (callId) => {
        log.push(`${callId}:start`);
        await new Promise((r) => setTimeout(r, 15));
        log.push(`${callId}:end`);
        return { ok: true, summary: callId };
      },
    });
    const writeTool = fakeTool("write", {
      execute: async (callId) => {
        log.push(`${callId}:start`);
        log.push(`${callId}:end`);
        return { ok: true, summary: callId };
      },
    });
    const registry = createToolRegistry([readTool, writeTool]);
    await registry.dispatch([call("read", "c1"), call("write", "c2")], ctx());
    expect(log).toEqual(["c1:start", "c1:end", "c2:start", "c2:end"]);
  });

  it("单调用失败只降级该调用，不拖垮整批（错误隔离）", async () => {
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async (callId) => {
        if (callId === "c2") throw new Error("boom");
        return { ok: true, summary: callId };
      },
    });
    const registry = createToolRegistry([tool]);
    const res = await registry.dispatch(
      [call("read", "c1"), call("read", "c2"), call("read", "c3")],
      ctx(),
    );
    expect(res.results.map((r) => ({ id: r.id, ok: r.ok }))).toEqual([
      { id: "c1", ok: true },
      { id: "c2", ok: false },
      { id: "c3", ok: true },
    ]);
  });

  it("中止后不再启动新调用、跳过结果回填", async () => {
    let executed = 0;
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async () => {
        executed++;
        return { ok: true, summary: "read" };
      },
    });
    const registry = createToolRegistry([tool]);
    const ac = new AbortController();
    ac.abort();
    const res = await registry.dispatch(
      [call("read", "c1"), call("read", "c2")],
      ctx(ac.signal),
    );
    expect(res.messages).toEqual([]);
    expect(res.results).toEqual([]);
    expect(res.outcomes).toEqual([]);
    expect(executed).toBe(0);
  });

  it("未知工具名报错回填，不中断其余调用（顺序保持）", async () => {
    const tool = fakeTool("read", { parallelSafe: true });
    const registry = createToolRegistry([tool]);
    const res = await registry.dispatch(
      [call("nope", "u1"), call("read", "c1")],
      ctx(),
    );
    expect(res.results[0].ok).toBe(false);
    expect(res.results[0].summary).toContain("未知工具");
    expect(res.results[1]).toEqual({ id: "c1", ok: true, summary: "read", detail: "read" });
  });

  it("连续多个非并行安全（读写）调用逐个执行（不丢调用）", async () => {
    const log: string[] = [];
    const writeTool = fakeTool("write", {
      execute: async (callId) => {
        log.push(callId);
        return { ok: true, summary: callId };
      },
    });
    const registry = createToolRegistry([writeTool]);
    const res = await registry.dispatch(
      [call("write", "w1"), call("write", "w2")],
      ctx(),
    );
    expect(log).toEqual(["w1", "w2"]);
    expect(res.results.map((r) => r.id)).toEqual(["w1", "w2"]);
  });

  it("中止发生在在飞时：在飞者收敛、不再启动新调用、结果跳过回填", async () => {
    let started = 0;
    const gate = deferred();
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async (callId) => {
        started++;
        await gate.promise;
        return { ok: true, summary: callId };
      },
    });
    const registry = createToolRegistry([tool]);
    const ac = new AbortController();
    const p = registry.dispatch(
      [call("read", "c1"), call("read", "c2"), call("read", "c3")],
      ctx(ac.signal),
      2, // 有界池：只有前两个在飞，c3 未启动
    );
    await tick();
    expect(started).toBe(2);
    ac.abort(); // 中止：c3 不再启动
    gate.resolve(); // 在飞者收敛
    const res = await p;
    expect(started).toBe(2); // c3 未被启动
    expect(res.results).toEqual([]); // 中止后全部跳过回填
  });

  it("空调用列表返回空结果", async () => {
    const registry = createToolRegistry([fakeTool("read")]);
    const res = await registry.dispatch([], ctx());
    expect(res.messages).toEqual([]);
    expect(res.results).toEqual([]);
    expect(res.outcomes).toEqual([]);
  });

  it("maxParallel=0 走守卫退化为串行", async () => {
    let active = 0;
    let maxActive = 0;
    const gate = deferred();
    const tool = fakeTool("read", {
      parallelSafe: true,
      execute: async () => {
        active++;
        maxActive = Math.max(maxActive, active);
        await gate.promise;
        active--;
        return { ok: true, summary: "read" };
      },
    });
    const registry = createToolRegistry([tool]);
    const p = registry.dispatch(
      [call("read", "c1"), call("read", "c2")],
      ctx(),
      0,
    );
    await tick();
    expect(maxActive).toBe(1);
    gate.resolve();
    await p;
  });
});
