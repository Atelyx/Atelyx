/**
 * 服务注册表查询（ctx.services）与可选依赖剥出测试。
 *
 * 验证：list() 返回当前已注册服务面（内核平台服务无提供者、插件提供服务带提供者插件 id、
 * builtins 领域服务经登记表补充归属）；get() 判空读取（未注册 = undefined，敏感面经审计
 * 包装记录摘要）；可选依赖 `{ optional: true }` 缺失不阻断激活（apply 照常执行，经
 * ctx.services.get 判空），必需依赖缺失仍按 inject 语义不激活。
 */
import { describe, expect, it, vi, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import { createKernel, getKernel, resetKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll, unmountPlugin } from "./loader";
import { registerServiceProvider } from "./services";
import { auditSnapshot, resetAudit } from "./audit";
import { splitInject } from "./loader";

vi.mock("@/services/native", () => ({
  nativeInvoke: vi.fn(async () => "ok"),
}));

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

declare module "@atelyx/cordis" {
  interface Context {
    svcA: { ping: () => string };
    svcB: { ping: () => string };
  }
}

let kernel: Kernel | null = null;

function makeKernel(): Kernel {
  kernel = createKernel();
  return kernel;
}

afterEach(async () => {
  if (kernel) {
    await unmountAll(kernel);
    resetKernel();
    kernel = null;
  }
  resetAudit();
});

describe("ctx.services.list", () => {
  it("内核平台服务不带提供者；插件 ctx.provide 的服务带提供者插件 id", async () => {
    const k = makeKernel();
    await mountPlugin(k, {
      id: "builtin.svc-a",
      apply: (ctx: Context) => {
        ctx.provide("svcA", { ping: () => "pong" });
      },
    });
    const list = k.ctx.services.list();
    // 内核平台服务（如 state/http）无 provider 字段。
    expect(list.find((s) => s.name === "state")).toEqual({ name: "state" });
    expect(list.find((s) => s.name === "http")).toEqual({ name: "http" });
    // 插件 ctx.provide 注册的服务：提供者 = 挂载 id。
    expect(list.find((s) => s.name === "svcA")).toEqual({ name: "svcA", provider: "builtin.svc-a" });
    // 服务注册表服务本身由内核提供（无提供者）。
    expect(list.find((s) => s.name === "services")).toEqual({ name: "services" });
    await unmountPlugin(k, "builtin.svc-a");
  });

  it("builtins 领域服务经登记表补充提供者（ctx.root.provide 的 root fiber 无归属）", async () => {
    const k = makeKernel();
    const off = registerServiceProvider("svcB", "builtin.table");
    const apply = (ctx: Context) => {
      ctx.root.provide("svcB", { ping: () => "pong" });
    };
    await mountPlugin(k, { id: "builtin.table", apply });
    const list = k.ctx.services.list();
    // 登记表补充的提供者（模拟 builtins 领域服务）。
    expect(list.find((s) => s.name === "svcB")).toEqual({ name: "svcB", provider: "builtin.table" });
    off();
    await unmountPlugin(k, "builtin.table");
  });

  it("卸载插件后其服务与提供者从 list 消失", async () => {
    const k = makeKernel();
    const off = registerServiceProvider("table", "builtin.table");
    const apply = (ctx: Context) => {
      ctx.provide("svcA", { ping: () => "pong" });
    };
    await mountPlugin(k, { id: "builtin.svc-a", apply });
    expect(k.ctx.services.list().some((s) => s.name === "svcA")).toBe(true);
    await unmountPlugin(k, "builtin.svc-a");
    expect(k.ctx.services.list().some((s) => s.name === "svcA")).toBe(false);
    off();
  });
});

describe("ctx.services.get", () => {
  it("已注册服务可读；未注册返回 undefined 不抛", async () => {
    const k = makeKernel();
    await mountPlugin(k, {
      id: "builtin.svc-a",
      apply: (ctx: Context) => {
        ctx.provide("svcA", { ping: () => "pong" });
      },
    });
    const svc = k.ctx.services.get("svcA");
    expect(svc).toBeDefined();
    expect((svc as { ping: () => string }).ping()).toBe("pong");
    expect(k.ctx.services.get("svcA" as never)).toBeDefined();
    expect(k.ctx.services.get("no-such" as never)).toBeUndefined();
    await unmountPlugin(k, "builtin.svc-a");
  });
});

describe("可选依赖剥出", () => {
  it("splitInject：可选标记条目剥出，必需依赖保留", () => {
    expect(splitInject(undefined)).toEqual({ required: undefined, optional: [] });
    expect(splitInject(["table"])).toEqual({ required: ["table"], optional: [] });
    expect(
      splitInject({ table: null, note: { optional: true } }),
    ).toEqual({ required: { table: null }, optional: ["note"] });
    expect(splitInject({ a: { optional: true }, b: { optional: true } })).toEqual({
      required: undefined,
      optional: ["a", "b"],
    });
  });

  it("可选依赖缺失不阻断激活：apply 照常执行，经 ctx.services.get 判空", async () => {
    const k = makeKernel();
    let applied = false;
    let got: unknown;
    const plugin = {
      inject: { svcA: { optional: true } }, // svcA 不存在：可选，不阻断。
      apply: (ctx: Context) => {
        applied = true;
        got = ctx.services.get("svcA");
      },
    };
    const result = await mountPlugin(k, { id: "builtin.opt", apply: plugin });
    expect(result).toEqual({ ok: true });
    expect(applied).toBe(true);
    expect(got).toBeUndefined();
    await unmountPlugin(k, "builtin.opt");
  });

  it("可选依赖存在时经 ctx.services.get 可读到", async () => {
    const k = makeKernel();
    await mountPlugin(k, {
      id: "builtin.svc-a",
      apply: (ctx: Context) => {
        ctx.provide("svcA", { ping: () => "pong" });
      },
    });
    let got: unknown;
    const plugin = {
      inject: { svcA: { optional: true } },
      apply: (ctx: Context) => {
        got = ctx.services.get("svcA");
      },
    };
    const result = await mountPlugin(k, { id: "builtin.opt", apply: plugin });
    expect(result).toEqual({ ok: true });
    expect((got as { ping: () => string }).ping()).toBe("pong");
    await unmountPlugin(k, "builtin.opt");
    await unmountPlugin(k, "builtin.svc-a");
  });

  it("必需依赖缺失仍不激活（inject 语义保持）", async () => {
    const k = makeKernel();
    let applied = false;
    const plugin = {
      inject: ["svcA"], // svcA 不存在：必需，不激活。
      apply: () => {
        applied = true;
      },
    };
    const result = await mountPlugin(k, { id: "builtin.req", apply: plugin });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.missing).toEqual(["svcA"]);
      expect(result.message).toContain("svcA");
    }
    expect(applied).toBe(false);
  });
});

describe("ctx.services.get 敏感面审计", () => {
  it("经 ctx.services.get 返回的敏感服务调用仍记脱敏摘要（与 ctx.<svc> 直连同口径）", async () => {
    kernel = getKernel(); // 应用路径：自动安装审计
    const apply = (ctx: Context) => {
      const svc = ctx.services.get("shell");
      void svc?.exec({ command: "cmd.exe", args: ["/C", "echo", "SECRET"] });
    };
    await mountPlugin(kernel, { id: "builtin.svc-a", apply });
    const entry = auditSnapshot(kernel.ctx).find((e) => e.pluginId === "builtin.svc-a");
    expect(entry?.calls).toEqual(
      expect.arrayContaining([{ service: "shell", method: "exec", summary: "cmd.exe（3 个参数）" }]),
    );
    const dumped = JSON.stringify(entry);
    expect(dumped).not.toContain("SECRET");
    await unmountPlugin(kernel, "builtin.svc-a");
  });
});
