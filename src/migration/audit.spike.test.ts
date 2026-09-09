/**
 * 插件审计可行性验证（「声明 vs 实际」语义的 Cordis 版）。
 * 机制候选：①包装 context 代理 handler.get 记录服务读（动态实际调用）；
 * ②读 events 注册表按 fiber 归属统计事件订阅。二者均不动框架源码。
 */
import { Context, ReflectService } from "@atelyx/cordis";
import { afterEach, describe, expect, it } from "vitest";

declare module "@atelyx/cordis" {
  interface Context {
    spikeAudit: { ping: () => string };
  }
  interface Events {
    "spike/audit": (msg: string) => void;
  }
}

describe("插件审计可行性", () => {
  const originalGet = ReflectService.handler.get!;
  afterEach(() => {
    ReflectService.handler.get = originalGet;
  });

  it("包装代理 get 可记录插件服务读", async () => {
    const reads: string[] = [];
    ReflectService.handler.get = function (target, prop, receiver) {
      const value = originalGet.call(this, target, prop, receiver);
      if (typeof prop === "string" && !prop.startsWith("_") && value !== undefined) {
        reads.push(prop);
      }
      return value;
    };

    const ctx = new Context();
    const fiber = ctx.plugin((c: Context) => {
      c.provide("spikeAudit", { ping: () => "pong" });
      void c.spikeAudit.ping();
    });
    await fiber.await();
    expect(reads).toContain("spikeAudit");
    await fiber.dispose();
  });

  it("事件订阅可按插件归属审计（hook.ctx.fiber.runtime 定位提供方）", async () => {
    const ctx = new Context();
    const pluginFn = (c: Context) => {
      c.on("spike/audit", () => {});
    };
    const fiber = ctx.plugin(pluginFn);
    await fiber.await();

    const subscribed: string[] = [];
    for (const name of Object.keys(ctx.events._hooks)) {
      const hooks = ctx.events._hooks[name];
      if (hooks.some((hook) => hook.ctx.fiber.runtime?.callback === pluginFn)) {
        subscribed.push(name);
      }
    }
    expect(subscribed).toContain("spike/audit");
    await fiber.dispose();
  });
});
