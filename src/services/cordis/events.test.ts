/**
 * 领域事件开放测试：插件订阅 typed events（note:changed/chat:message 等）并收到回调；
 * 卸载后订阅随 fiber 撤销（不再收到）；单个监听器抛错不打断同事件其余监听器（逐监听器隔离）。
 */
import { describe, expect, it, afterEach, vi } from "vitest";
import type { Context } from "@atelyx/cordis";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import { emitPluginEvent, runSerialHook, setKernelRef } from "./events";

let kernel: Kernel | null = null;

async function boot(): Promise<Kernel> {
  const k = createKernel();
  setKernelRef(k);
  kernel = k;
  return k;
}

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    setKernelRef(null);
    kernel.dispose();
    kernel = null;
  }
});

describe("领域事件开放", () => {
  it("插件订阅 note:changed/chat:message 收到回调；卸载后撤销", async () => {
    const k = await boot();
    const received: Array<{ event: string; payload: unknown }> = [];
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:changed", (p) => {
          received.push({ event: "note:changed", payload: p });
        }),
      );
      ctx.effect(() =>
        ctx.events.on("chat:message", (p) => {
          received.push({ event: "chat:message", payload: p });
        }),
      );
    };
    const result = await mountPlugin(k, { id: "com.test.watch", apply });
    expect(result.ok).toBe(true);

    emitPluginEvent("note:changed", { file: "笔记.md" });
    emitPluginEvent("chat:message", { sessionId: "s1", role: "assistant", content: "hi" });
    expect(received).toHaveLength(2);
    expect(received[0]).toMatchObject({ event: "note:changed", payload: { file: "笔记.md" } });
    expect(received[1]).toMatchObject({ event: "chat:message", payload: { sessionId: "s1", role: "assistant", content: "hi" } });

    // 卸载：subscriptions 随 fiber dispose 撤销。
    await unmountAll(k);
    received.length = 0;
    emitPluginEvent("note:changed", { file: "x.md" });
    expect(received).toHaveLength(0);
  });

  it("单个监听器抛错不打断同事件其余监听器（同步与异步）", async () => {
    const k = await boot();
    const received: string[] = [];
    const errors: unknown[][] = [];
    const errorSpy = vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
      errors.push(args);
    });
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:changed", () => {
          throw new Error("插件甲处理失败");
        }),
      );
      ctx.effect(() =>
        ctx.events.on("note:changed", () => Promise.reject(new Error("插件乙异步失败"))),
      );
      ctx.effect(() => ctx.events.on("note:changed", () => received.push("丙")));
    };
    const result = await mountPlugin(k, { id: "com.test.isolate", apply });
    expect(result.ok).toBe(true);

    // 抛错的甲在前：乙（异步拒绝）与丙仍须被投递
    emitPluginEvent("note:changed", { file: "a.md" });
    await Promise.resolve();
    await Promise.resolve();
    expect(received).toEqual(["丙"]);
    expect(errors).toHaveLength(2);
    // 日志必须带事件名（dispatch 会就地消费 args，事件名需在派发前取，否则只剩载荷无法定位）
    for (const call of errors) {
      expect(call.map((part) => String(part)).join(" ")).toContain("note:changed");
    }
    errorSpy.mockRestore();
  });
});

describe("runSerialHook（serial veto/改写分派）", () => {
  it("内核未建：no-op 原样返回", async () => {
    const { vetoed, payload } = await runSerialHook("note:before-save", {
      file: "a.md",
      content: "x",
    });
    expect(vetoed).toBe(false);
    expect(payload).toEqual({ file: "a.md", content: "x" });
  });

  it("顺序执行且改写传递：后一个监听器拿到前一个的改写载荷", async () => {
    const k = await boot();
    const seen: string[] = [];
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => {
          seen.push(p.content);
          return { content: `A:${p.content}` };
        }),
      );
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => {
          seen.push(p.content);
          return { content: `B:${p.content}` };
        }),
      );
    };
    await mountPlugin(k, { id: "com.test.serial", apply });

    const { vetoed, payload } = await runSerialHook("note:before-save", {
      file: "a.md",
      content: "x",
    });
    expect(vetoed).toBe(false);
    expect(payload).toEqual({ file: "a.md", content: "B:A:x" });
    expect(seen).toEqual(["x", "A:x"]);
  });

  it("veto 短路：返回 { veto: true } 后后续监听器不再执行，载荷停在 veto 前", async () => {
    const k = await boot();
    const seen: string[] = [];
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => {
          seen.push("first");
          return { content: `改写:${p.content}` };
        }),
      );
      ctx.effect(() =>
        ctx.events.on("note:before-save", () => {
          seen.push("veto");
          return { veto: true };
        }),
      );
      ctx.effect(() =>
        ctx.events.on("note:before-save", () => {
          seen.push("third");
        }),
      );
    };
    await mountPlugin(k, { id: "com.test.veto", apply });

    const { vetoed, payload } = await runSerialHook("note:before-save", {
      file: "a.md",
      content: "x",
    });
    expect(vetoed).toBe(true);
    expect(payload.content).toBe("改写:x"); // 停在 veto 前的改写值
    expect(seen).toEqual(["first", "veto"]); // third 未执行
  });

  it("监听器抛错：异常隔离，跳过并继续其余监听器，改写链不受影响", async () => {
    const k = await boot();
    const errors: unknown[][] = [];
    const errorSpy = vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
      errors.push(args);
    });
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", () => {
          throw new Error("插件抛错");
        }),
      );
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => ({ content: `改写:${p.content}` })),
      );
    };
    await mountPlugin(k, { id: "com.test.err", apply });

    const { vetoed, payload } = await runSerialHook("note:before-save", {
      file: "a.md",
      content: "x",
    });
    expect(vetoed).toBe(false);
    expect(payload.content).toBe("改写:x"); // 抛错监听器不参与改写，后续正常
    expect(errors).toHaveLength(1);
    expect(errors[0].map((part) => String(part)).join(" ")).toContain("note:before-save");
    errorSpy.mockRestore();
  });

  it("卸载后监听器随 fiber 撤销：不再拦截", async () => {
    const k = await boot();
    const apply = (ctx: Context) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => ({ content: `改写:${p.content}` })),
      );
    };
    await mountPlugin(k, { id: "com.test.unload", apply });
    const hooked = await runSerialHook("note:before-save", { file: "a.md", content: "x" });
    expect(hooked.payload.content).toBe("改写:x");

    await unmountAll(k);
    const { vetoed, payload } = await runSerialHook("note:before-save", {
      file: "a.md",
      content: "x",
    });
    expect(vetoed).toBe(false);
    expect(payload.content).toBe("x");
  });
});
