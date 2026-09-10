/**
 * 领域事件开放测试：插件订阅 typed events（note:changed/chat:message 等）并收到回调；
 * 卸载后订阅随 fiber 撤销（不再收到）。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import { emitPluginEvent, setKernelRef } from "./events";

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
});
