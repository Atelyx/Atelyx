/**
 * 插件审计测试（services/cordis/audit）。
 *
 * 验证「声明 vs 实际」的实际侧：ctx 服务读（包装代理 get，按插件归属）+ 事件订阅
 * （events._hooks 按插件上下文归属）。归属经 loader 挂载时登记的 contextToPluginId。
 * 另锁「服务面清单 ↔ 内核 ctx 服务契约」一致（漂移即失败），以及卸载后记录清理。
 */
import { describe, expect, it, afterEach } from "vitest";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import type { Context } from "@atelyx/cordis";
import { getKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll, unmountPlugin } from "./loader";
import { auditSnapshot, resetAudit } from "./audit";
import { PLUGIN_SERVICE_NAMES } from "@/constants/pluginServices";

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

  it("slots 纳入审计（插件经 ctx.slots 注册视图/命令不再隐形）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.slots;
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const snap = auditSnapshot(kernel.ctx);
    expect(snap.find((e) => e.pluginId === "builtin.audit")?.services).toContain("slots");
  });

  it("卸载后该插件的审计记录被清理（不跨卸载/换包累积）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.vault;
      ctx.on("audit/evt", () => {});
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    expect(auditSnapshot(kernel.ctx).some((e) => e.pluginId === "builtin.audit")).toBe(true);

    await unmountPlugin(kernel, "builtin.audit");
    expect(auditSnapshot(kernel.ctx).some((e) => e.pluginId === "builtin.audit")).toBe(false);
  });

  it("挂载失败的插件同样不留审计记录（失败行不在已挂载表内，卸载路径清不到）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.vault;
      throw new Error("初始化失败");
    };
    const result = await mountPlugin(kernel, { id: "builtin.audit", apply });
    expect(result.ok).toBe(false);
    expect(auditSnapshot(kernel.ctx).some((e) => e.pluginId === "builtin.audit")).toBe(false);
  });

  it("服务面清单与内核 ctx 类型声明一致（两处枚举不得漂移）", async () => {
    // 口径 = `types.ts` 的服务声明（ctx 服务面契约）；kernel 的 provide 面与插件提供的领域服务
    // 不在本断言范围（服务改名时须同时改 types.ts 与标签清单，否则此测试失败）
    const typesSource = await readFile(resolve(process.cwd(), "src/services/cordis/types.ts"), "utf8");
    // 契约声明形态：declare module "@atelyx/cordis" { interface Context { <name>: <Type>; ... } }
    const declaration = typesSource
      .split('declare module "@atelyx/cordis"')[1]
      ?.split("interface Context {")[1]
      ?.split("}")[0];
    expect(declaration, "未找到 declare module 内的 interface Context 声明").toBeTruthy();
    const declared = [...declaration!.matchAll(/^\s*([A-Za-z][A-Za-z0-9]*)\s*:/gmu)].map((m) => m[1]!);
    expect([...declared].sort()).toEqual([...PLUGIN_SERVICE_NAMES].sort());
  });
});
