/**
 * 插件审计测试（services/cordis/audit）。
 *
 * 验证「声明 vs 实际」的实际侧：ctx 服务读（包装代理 get，按插件归属）+ 事件订阅
 * （events._hooks 按插件上下文归属）+ 高危调用摘要（脱敏，不含参数原文）。归属经 loader
 * 挂载时登记的 contextToPluginId。另锁卸载后记录清理。服务面清单与 ctx 契约的一致性由
 * `scripts/gen-ctx-api.mjs` 门禁把守（`pnpm run ctx-api:check`）。
 */
import { describe, expect, it, afterEach, vi } from "vitest";
import type { Context } from "@atelyx/cordis";
import { getKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll, unmountPlugin } from "./loader";
import { auditSnapshot, resetAudit } from "./audit";
import { PLUGIN_SENSITIVE_METHODS, PLUGIN_SERVICE_NAMES } from "@/constants/pluginServices";

// 高危服务调用会真的打到 Tauri：替换为无副作用替身，只验证摘要记录。
vi.mock("@/services/shell", () => ({
  openInExplorer: () => Promise.resolve(),
  openUrl: () => Promise.resolve(),
  runProcess: (
    _program: string,
    _args: string[],
    _options: unknown,
    handlers: { close: (code: number | null) => void },
  ) => {
    handlers.close(0);
    return { cancel: () => {} };
  },
}));

vi.mock("@/services/http", () => ({
  httpRequest: () => Promise.resolve({ status: 200, headers: {}, body: "", truncated: false }),
}));

vi.mock("@/services/clipboard", () => ({
  readClipboardText: () => Promise.resolve(""),
  writeClipboardText: () => Promise.resolve(),
  copyImageToClipboard: () => Promise.resolve(),
}));

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

  it("高危服务调用按插件归属记脱敏摘要（只记形状与规模）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.shell.exec({ command: "cmd.exe", args: ["/C", "echo", "TOP SECRET"] });
      void ctx.clipboard.writeText("TOP SECRET");
      void ctx.http.request({
        url: "https://api.example.com/v1/x?token=SECRET",
        method: "POST",
        body: "TOP SECRET",
      });
      try {
        void ctx.vault.writeFile("notes/a.md", "TOP SECRET");
      } catch {
        // 写面未接线：摘要应在转发前已记录。
      }
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const entry = auditSnapshot(kernel.ctx).find((e) => e.pluginId === "builtin.audit");
    expect(entry?.calls).toEqual(
      expect.arrayContaining([
        { service: "shell", method: "exec", summary: "cmd.exe（3 个参数）" },
        { service: "clipboard", method: "writeText", summary: "writeText（10 字节）" },
        { service: "http", method: "request", summary: "POST https://api.example.com/v1/x" },
        { service: "vault", method: "writeFile", summary: "writeFile notes/a.md" },
      ]),
    );
  });

  it("摘要不泄漏参数原文（正文 / 凭据不进审计）", async () => {
    kernel = getKernel();
    const apply = (ctx: Context) => {
      void ctx.clipboard.writeText("TOP SECRET");
      void ctx.http.request({ url: "https://api.example.com/v1/x?token=SECRET", body: "TOP SECRET" });
      try {
        void ctx.vault.writeFile("notes/a.md", "TOP SECRET");
      } catch {
        // 写面未接线：摘要应在转发前已记录。
      }
    };
    await mountPlugin(kernel, { id: "builtin.audit", apply });
    const entry = auditSnapshot(kernel.ctx).find((e) => e.pluginId === "builtin.audit");
    const dumped = JSON.stringify(entry);
    expect(dumped).not.toContain("TOP SECRET");
    expect(dumped).not.toContain("token");
  });

  it("方法级敏感面清单不漂移（键须是已登记服务）", () => {
    expect(Object.keys(PLUGIN_SENSITIVE_METHODS).every((s) => PLUGIN_SERVICE_NAMES.includes(s))).toBe(true);
  });
});
