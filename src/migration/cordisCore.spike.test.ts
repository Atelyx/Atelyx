/**
 * vendored Cordis 核心可运行性验证（核心进本项目测试管线的回归锚点）。
 * 验证：核心在 vitest（node 环境、经 Vite 转换管线）可启动；typed events 声明合并、
 * 服务提供/读取/撤销、waterfall 环绕、插件注册随 fiber 卸载可逆撤销。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it } from "vitest";

/** typed events 声明合并：扩展点即类型系统（本文件演示合并语法）。 */
declare module "@atelyx/cordis" {
  interface Context {
    spikeNote: { append: (text: string) => void };
  }
  interface Events {
    "spike/emit": (msg: string) => void;
    "spike/wf": (value: string, next: (value?: string) => string) => string;
  }
}

describe("vendored Cordis 核心可运行", () => {
  it("根 Context 启动且基础服务就绪", () => {
    const ctx = new Context();
    expect(ctx.events).toBeDefined();
    expect(ctx.registry).toBeDefined();
    expect(ctx.reflect).toBeDefined();
  });

  it("服务提供/读取 + 撤销", () => {
    const ctx = new Context();
    const dispose = ctx.provide("spikeNote", { append: () => {} });
    expect(ctx.spikeNote).toBeDefined();
    dispose();
    expect(ctx.get("spikeNote")).toBeUndefined();
  });

  it("typed event：emit/on/once", () => {
    const ctx = new Context();
    const seen: string[] = [];
    ctx.on("spike/emit", (msg) => {
      seen.push(`on:${msg}`);
    });
    ctx.once("spike/emit", (msg) => {
      seen.push(`once:${msg}`);
    });
    ctx.emit("spike/emit", "a");
    ctx.emit("spike/emit", "b");
    expect(seen).toEqual(["on:a", "once:a", "on:b"]);
  });

  it("waterfall 环绕：next 续链", () => {
    const ctx = new Context();
    ctx.on("spike/wf", (value, next) => `[${next(value)}]`);
    const result = ctx.waterfall("spike/wf", "v", (value) => `${value}!`);
    expect(result).toBe("[v!]");
  });

  it("插件挂载 + 注册随 fiber 卸载可逆撤销", async () => {
    const ctx = new Context();
    const seen: string[] = [];
    const fiber = ctx.plugin((c: Context) => {
      c.provide("spikeNote", { append: (text) => { seen.push(text); } });
      c.on("spike/emit", (msg) => { seen.push(msg); });
    });
    await fiber.await();
    ctx.spikeNote.append("x");
    ctx.emit("spike/emit", "y");
    expect(seen).toEqual(["x", "y"]);
    await fiber.dispose();
    expect(ctx.get("spikeNote")).toBeUndefined();
    ctx.emit("spike/emit", "z");
    expect(seen).toEqual(["x", "y"]);
  });
});
