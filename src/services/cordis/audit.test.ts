/**
 * 插件审计测试（services/cordis/audit）。
 *
 * 验证「声明 vs 实际」的实际侧：ctx 服务读（包装代理 get，按插件归属）+ 事件订阅
 * （events._hooks 按插件上下文归属）。归属经 loader 挂载时登记的 contextToPluginId。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { getKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll } from "./loader";
import { auditSnapshot, resetAudit } from "./audit";

declare module "@atelyx/cordis" {
  interface Events {
    "audit/evt": (msg: string) => void;
  }
}

let kernel: Kernel | null = null;

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    resetKernel();
    kernel = null;
  }
  resetAudit();
});

describe("插件审计", () => {
  it("ctx 服务读按插件归属记录（只读属性访问即记录，不触发方法）", async () => {
    kernel = getKernel(); // 应用路径：自动安装审计
    const apply = (ctx: Context) => {
      void ctx.vault;
      void ctx.collab;
      void ctx.ai;
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const snap = auditSnapshot(kernel.ctx);
    const entry = snap.find((e) => e.pluginId === "builtin.audit");
    expect(entry?.services.sort()).toEqual(["ai", "collab", "vault"]);
  });

  it("事件订阅按插件归属记录（ctx.on 的 hook.ctx 归属）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      ctx.on("audit/evt", () => {});
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const snap = auditSnapshot(kernel.ctx);
    const entry = snap.find((e) => e.pluginId === "builtin.audit");
    expect(entry?.events).toContain("audit/evt");
  });

  it("服务读与事件订阅聚合到同一插件条目", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.vault;
      ctx.on("audit/evt", () => {});
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const snap = auditSnapshot(kernel.ctx);
    const entry = snap.find((e) => e.pluginId === "builtin.audit");
    expect(entry?.services).toContain("vault");
    expect(entry?.events).toContain("audit/evt");
  });
});
