/**
 * vendored Cordis 核心行为锚点（`vendor/cordis` 同步/升级前先跑这条）。
 *
 * 验证核心在 vitest（node 环境、经 Vite 转换管线）可启动：typed events 声明合并、
 * 服务提供/读取/撤销、on/once、waterfall 环绕、插件注册随 fiber 卸载可逆撤销。
 * 与 kernel/loader/slotsApi 等测试的分工：那些走宿主封装，本文件只钉 vendor 自身语义。
 */
import { Context } from "@atelyx/cordis";
import { describe, expect, it } from "vitest";

/** typed events 声明合并：扩展点即类型系统（本文件演示合并语法）。 */
declare module "@atelyx/cordis" {
  interface Context {
    vendorNote: { append: (text: string) => void };
  }
  interface Events {
    "vendor/emit": (msg: string) => void;
    "vendor/wf": (value: string, next: (value?: string) => string) => string;
  }
}

describe("vendored Cordis 核心行为", () => {
  it("根 Context 启动且基础服务就绪", () => {
    const ctx = new Context();
    expect(ctx.events).toBeDefined();
    expect(ctx.registry).toBeDefined();
    expect(ctx.reflect).toBeDefined();
  });

  it("服务提供/读取 + 撤销", () => {
    const ctx = new Context();
    const dispose = ctx.provide("vendorNote", { append: () => {} });
    expect(ctx.vendorNote).toBeDefined();
    dispose();
    expect(ctx.get("vendorNote")).toBeUndefined();
  });

  it("typed event：emit/on/once", () => {
    const ctx = new Context();
    const seen: string[] = [];
    ctx.on("vendor/emit", (msg) => {
      seen.push(`on:${msg}`);
    });
    ctx.once("vendor/emit", (msg) => {
      seen.push(`once:${msg}`);
    });
    ctx.emit("vendor/emit", "a");
    ctx.emit("vendor/emit", "b");
    expect(seen).toEqual(["on:a", "once:a", "on:b"]);
  });

  it("waterfall 环绕：next 续链", () => {
    const ctx = new Context();
    ctx.on("vendor/wf", (value, next) => `[${next(value)}]`);
    const result = ctx.waterfall("vendor/wf", "v", (value) => `${value}!`);
    expect(result).toBe("[v!]");
  });

  it("插件挂载 + 注册随 fiber 卸载可逆撤销", async () => {
    const ctx = new Context();
    const seen: string[] = [];
    const fiber = ctx.plugin((c: Context) => {
      c.provide("vendorNote", { append: (text) => { seen.push(text); } });
      c.on("vendor/emit", (msg) => { seen.push(msg); });
    });
    await fiber.await();
    ctx.vendorNote.append("x");
    ctx.emit("vendor/emit", "y");
    expect(seen).toEqual(["x", "y"]);
    await fiber.dispose();
    expect(ctx.get("vendorNote")).toBeUndefined();
    ctx.emit("vendor/emit", "z");
    expect(seen).toEqual(["x", "y"]);
  });
});
