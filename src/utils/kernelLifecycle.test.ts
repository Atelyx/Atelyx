/**
 * 内核生命周期注册表测试：
 * 注册/撤销/同 id 覆盖/分发顺序/fail-fast 传播/空注册表退化。
 */
import { describe, it, expect } from "vitest";
import {
  flushAllDomains,
  notifyVaultEntered,
  notifyVaultExit,
  notifyVaultLeaving,
  notifyViewGained,
  notifyViewRemoved,
  registerDomainLifecycle,
  releaseView,
  unregisterDomainLifecycle,
  hasDomainLifecycle,
} from "./kernelLifecycle";

describe("领域生命周期注册表", () => {
  it("按注册序分发 flush / entered / exit / leaving / releaseView / 视图进出", async () => {
    const order: string[] = [];
    const a = registerDomainLifecycle({
      id: "a",
      flush: async () => {
        order.push("a.flush");
      },
      onVaultLeaving: () => {
        order.push("a.leaving");
      },
      onVaultEntered: async () => {
        order.push("a.entered");
      },
      onVaultExit: async () => {
        order.push("a.exit");
      },
      releaseView: async (v) => {
        order.push(`a.release:${v}`);
      },
      onViewGained: (v) => {
        order.push(`a.gained:${v}`);
      },
      onViewRemoved: (v) => {
        order.push(`a.removed:${v}`);
      },
    });
    const b = registerDomainLifecycle({
      id: "b",
      flush: async () => {
        order.push("b.flush");
      },
      releaseView: async (v) => {
        order.push(`b.release:${v}`);
      },
    });

    await flushAllDomains({ vaultId: "v1" });
    expect(order).toEqual(["a.flush", "b.flush"]);

    order.length = 0;
    notifyVaultLeaving();
    expect(order).toEqual(["a.leaving"]);

    order.length = 0;
    await notifyVaultEntered({ vaultId: "v1" });
    expect(order).toEqual(["a.entered"]);

    order.length = 0;
    await notifyVaultExit();
    expect(order).toEqual(["a.exit"]);

    order.length = 0;
    await releaseView("canvas");
    expect(order).toEqual(["a.release:canvas", "b.release:canvas"]);

    order.length = 0;
    notifyViewGained("aichat");
    notifyViewRemoved("canvas");
    expect(order).toEqual(["a.gained:aichat", "a.removed:canvas"]);

    a();
    b();
  });

  it("撤销后不再分发；同 id 后注册者生效", async () => {
    const calls: string[] = [];
    const r1 = registerDomainLifecycle({
      id: "x",
      flush: async () => {
        calls.push("first");
      },
    });
    const r2 = registerDomainLifecycle({
      id: "x",
      flush: async () => {
        calls.push("second");
      },
    });
    await flushAllDomains({ vaultId: null });
    expect(calls).toEqual(["second"]); // 同 id 覆盖，旧注册被顶替

    r1(); // 撤销旧句柄不应误删新注册（按引用守卫）
    calls.length = 0;
    await flushAllDomains({ vaultId: null });
    expect(calls).toEqual(["second"]);

    r2();
    calls.length = 0;
    await flushAllDomains({ vaultId: null });
    expect(calls).toEqual([]);

    unregisterDomainLifecycle("x"); // 幂等
    expect(hasDomainLifecycle("x")).toBe(false);
  });

  it("flush 失败快速传播（不吞错），后续钩子不执行", async () => {
    const after: string[] = [];
    const r1 = registerDomainLifecycle({
      id: "boom",
      flush: async () => {
        throw new Error("flush 失败");
      },
    });
    registerDomainLifecycle({
      id: "after",
      flush: async () => {
        after.push("after");
      },
    });
    await expect(flushAllDomains({ vaultId: null })).rejects.toThrow("flush 失败");
    expect(after).toEqual([]);
    r1();
    unregisterDomainLifecycle("after");
  });

  it("空注册表分发全部为 no-op", async () => {
    unregisterDomainLifecycle("a");
    unregisterDomainLifecycle("b");
    unregisterDomainLifecycle("x");
    await expect(flushAllDomains({ vaultId: null })).resolves.toBeUndefined();
    expect(hasDomainLifecycle("any")).toBe(false);
    notifyVaultLeaving();
    await notifyVaultEntered({ vaultId: "v" });
    await notifyVaultExit();
    await releaseView("table");
    notifyViewGained("canvas");
    notifyViewRemoved("note");
  });

  it("分发上下文透传 vaultId；未实现字段跳过", async () => {
    const got: unknown[] = [];
    const r = registerDomainLifecycle({
      id: "ctx",
      flush: async (ctx) => {
        got.push(ctx.vaultId);
      },
    });
    await flushAllDomains({ vaultId: "vault-42" });
    expect(got).toEqual(["vault-42"]);
    r();
    unregisterDomainLifecycle("ctx");
  });

  it("进入仓库后加载领域上下文；上下文透传", async () => {
    const got: unknown[] = [];
    const r = registerDomainLifecycle({
      id: "entered",
      onVaultEntered: async (ctx) => {
        got.push(ctx.vaultId);
      },
    });
    await notifyVaultEntered({ vaultId: "vault-7" });
    expect(got).toEqual(["vault-7"]);
    r();
    unregisterDomainLifecycle("entered");
  });

  it("releaseView 只调用实现了 releaseView 的钩子（各自判断 view）", async () => {
    const called: string[] = [];
    const r = registerDomainLifecycle({
      id: "canvas-only",
      releaseView: async (view) => {
        if (view === "canvas") called.push("canvas");
      },
    });
    registerDomainLifecycle({
      id: "table-only",
      releaseView: async (view) => {
        if (view === "table") called.push("table");
      },
    });
    await releaseView("canvas");
    expect(called).toEqual(["canvas"]);
    r();
    unregisterDomainLifecycle("table-only");
  });
});
