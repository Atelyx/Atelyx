/**
 * ctx.slots 注册 API 测试（services/cordis/slotsApi）。
 *
 * 覆盖：插件 apply 内经 ctx.slots.registerView/registerTableView 注册生效、pluginId 归属正确、
 * 卸载随 fiber 撤销（tracker 绑定调用方插件上下文）；插件自声明槽（declare）先到先得 +
 * 宿主保护 + 冲突指名占用者；host(slot) 托管语义。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { Context } from "@atelyx/cordis";
import type { SlotDeclaration } from "@/constants/slots";
import { createKernel, type Kernel } from "./kernel";
import { mountPlugin, unmountAll, unmountPlugin } from "./loader";
import { resolveViewKind, viewKinds, listSlot, listDecorators, registeredSlots, onSlotChange, findSlotDeclarationRuntime } from "./slots";
import { getPluginTableView } from "./ui";
import { setPluginSlotHostComponent } from "./access";

let kernel: Kernel | null = null;

afterEach(async () => {
  setPluginSlotHostComponent(null);
  if (kernel) {
    await unmountAll(kernel);
    kernel.dispose();
    kernel = null;
  }
});

describe("ctx.slots", () => {
  it("registerView：注册视图槽 + pluginId 归属；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerView({ kind: "com.test.panel", label: "面板", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(viewKinds()).toContain("com.test.panel");
    expect(resolveViewKind("com.test.panel")?.pluginId).toBe("com.test.ui");
    expect(resolveViewKind("com.test.panel")?.payload.label).toBe("面板");

    await unmountAll(kernel);
    expect(viewKinds()).toEqual([]);
  });

  it("registerTableView：表格视图注册；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerTableView({ kind: "com.test.tl", label: "时间线", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(getPluginTableView("com.test.tl")?.pluginId).toBe("com.test.ui");
    expect(getPluginTableView("com.test.tl")?.label).toBe("时间线");

    await unmountAll(kernel);
    expect(getPluginTableView("com.test.tl")).toBeUndefined();
  });

  it("registerView 缺 component/render 拒绝", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      // 缺 component 与 render：注册时报错 → apply 失败可见。
      ctx.slots.registerView({ kind: "com.test.bad", label: "坏" });
    };
    const result = await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(result.ok).toBe(false);
    expect(viewKinds()).toEqual([]);
  });

  it("registerUi：可向已声明的具名槽位贡献组件；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerUi({ slot: "toolbar/note/right", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(registeredSlots()).toContain("toolbar/note/right");
    expect(listSlot("toolbar/note/right")).toHaveLength(1);

    await unmountAll(kernel);
    expect(listSlot("toolbar/note/right")).toHaveLength(0);
  });

  it("registerMenu：向已声明的菜单目标贡献菜单项；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerMenu({ target: "canvas", label: "统计选中", onClick: () => undefined });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(listSlot("contextmenu/canvas")).toHaveLength(1);

    await unmountAll(kernel);
    expect(listSlot("contextmenu/canvas")).toHaveLength(0);
  });

  it("registerMenu：多贡献按 priority 降序", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerMenu({ target: "canvas", label: "低", onClick: () => undefined });
      ctx.slots.registerMenu({ target: "canvas", label: "高", onClick: () => undefined, priority: 5 });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    const labels = listSlot("contextmenu/canvas").map((c) => (c.payload as { label: string }).label);
    expect(labels).toEqual(["高", "低"]);
  });

  it("registerMenu 进未声明的菜单目标 → 该行 failed + 提示可用目标", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerMenu({ target: "unknown", label: "打开", onClick: () => undefined });
    };
    const result = await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("未声明的槽位");
      expect(result.message).toContain("contextmenu/canvas");
    }
    expect(listSlot("contextmenu/unknown")).toEqual([]);
  });

  it("list：返回宿主槽位声明表（插件可发现可贡献的位置）", async () => {
    kernel = createKernel();
    let seen: readonly SlotDeclaration[] = [];
    await mountPlugin(kernel, {
      id: "com.test.ui",
      apply: (ctx) => {
        seen = ctx.slots.list();
      },
    });
    expect(seen.some((d) => d.key === "toolbar/note/right")).toBe(true);
    expect(seen.some((d) => d.key === "view" && d.prefix === true)).toBe(true);
    // 返回冻结合并视图：数组与元素均不可被插件改写（防篡改全局校验依据）。
    expect(Object.isFrozen(seen)).toBe(true);
    for (const decl of seen) expect(Object.isFrozen(decl), decl.key).toBe(true);
  });

  it("registerUi 进未声明的固定槽 → 该行 failed + 可读原因（不静默丢失）", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.registerUi({ slot: "toolbar/notes/right", component: () => null });
    };
    const result = await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("未声明的槽位");
      expect(result.message).toContain("toolbar/note/right");
    }
    expect(listSlot("toolbar/notes/right")).toEqual([]);
  });
});

describe("载荷与参数契约", () => {
  it("载荷字段值类型不符即拒绝：数字 label / 非函数 component / 非函数 onClick", async () => {
    const cases: { name: string; register: (ctx: Context) => void; expect: string }[] = [
      {
        name: "数字 label",
        register: (ctx) => ctx.slots.registerView({ kind: "com.test.t", label: 1 as never, component: () => null }),
        expect: "label",
      },
      {
        name: "非函数 component",
        register: (ctx) => ctx.slots.registerView({ kind: "com.test.t", label: "x", component: "nope" as never }),
        expect: "component",
      },
      {
        name: "非函数 onClick",
        register: (ctx) => ctx.slots.registerMenu({ target: "canvas", label: "x", onClick: "nope" as never }),
        expect: "onClick",
      },
    ];
    for (const c of cases) {
      kernel = createKernel();
      const result = await mountPlugin(kernel, { id: "com.test.ui", apply: c.register });
      expect(result.ok, c.name).toBe(false);
      if (!result.ok) expect(result.message, c.name).toContain("类型不符");
      expect(viewKinds(), c.name).toEqual([]);
      await unmountAll(kernel);
      kernel = null;
    }
  });

  it("priority 非数字即拒绝（NaN 含内）", async () => {
    for (const priority of ["5" as never, Number.NaN as never]) {
      kernel = createKernel();
      const result = await mountPlugin(kernel, {
        id: "com.test.ui",
        apply: (ctx) => ctx.slots.registerMenu({ target: "canvas", label: "x", onClick: () => undefined, priority }),
      });
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.message).toContain("priority 须为数字");
      await unmountAll(kernel);
      kernel = null;
    }
  });
});

describe("ctx.slots.decorate", () => {
  it("decorate：向已声明槽注册装饰器 + pluginId 归属；卸载撤销", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.decorate({ slot: "toolbar/note/right", wrapper: ({ children }) => children });
    };
    await mountPlugin(kernel, { id: "com.test.deco", apply });
    const decos = listDecorators("toolbar/note/right");
    expect(decos).toHaveLength(1);
    expect(decos[0]?.pluginId).toBe("com.test.deco");
    expect(decos[0]?.slot).toBe("toolbar/note/right");

    await unmountAll(kernel);
    expect(listDecorators("toolbar/note/right")).toEqual([]);
  });

  it("decorate：装饰器按 priority 降序（外层 = 高 priority）", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.decorate({ slot: "toolbar/note/right", wrapper: () => null, priority: 1 });
      ctx.slots.decorate({ slot: "toolbar/note/right", wrapper: () => null, priority: 5 });
    };
    await mountPlugin(kernel, { id: "com.test.deco", apply });
    expect(listDecorators("toolbar/note/right").map((d) => d.priority)).toEqual([5, 1]);
  });

  it("decorate 进未声明的槽 → 该行 failed", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.decorate({ slot: "toolbar/none", wrapper: () => null });
    };
    const result = await mountPlugin(kernel, { id: "com.test.deco", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("未声明的槽位");
  });

  it("decorate 结构敏感槽（decoratable:false）→ 该行 failed", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.decorate({ slot: "contextmenu/canvas", wrapper: () => null });
    };
    const result = await mountPlugin(kernel, { id: "com.test.deco", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("不可被装饰");
  });

  it("decorate 缺包裹组件 → 拒绝", async () => {
    kernel = createKernel();
    const apply = (ctx: Context) => {
      ctx.slots.decorate({ slot: "toolbar/note/right", wrapper: undefined as never });
    };
    const result = await mountPlugin(kernel, { id: "com.test.deco", apply });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("包裹组件");
  });
});

describe("槽注册变更通知", () => {
  it("全局 onSlotChange 收到变化槽名（pluginStore 据此 bump 按槽修订，驱动宿主按槽订阅）", async () => {
    kernel = createKernel();
    const seen: string[] = [];
    const unsubscribe = onSlotChange((slot) => seen.push(slot));
    await mountPlugin(kernel, {
      id: "com.test.ui",
      apply: (ctx) => {
        ctx.slots.registerUi({ slot: "toolbar/files", component: () => null });
        ctx.slots.registerUi({ slot: "toolbar/note/right", component: () => null });
      },
    });
    expect(seen).toContain("toolbar/files");
    expect(seen).toContain("toolbar/note/right");
    unsubscribe();
  });

  it("成功注册通知一次、失败不通知、撤销命中通知一次", async () => {
    kernel = createKernel();
    let notified = 0;
    const unsubscribe = onSlotChange(() => {
      notified += 1;
    });
    const apply = (ctx: Context) => {
      ctx.slots.registerUi({ slot: "toolbar/files", component: () => null });
    };
    await mountPlugin(kernel, { id: "com.test.ui", apply });
    expect(notified).toBe(1);

    await unmountAll(kernel);
    kernel = null;
    expect(notified).toBe(2);
    unsubscribe();
  });

  it("注册失败（未声明槽位）不触发通知", async () => {
    kernel = createKernel();
    let notified = 0;
    const unsubscribe = onSlotChange(() => {
      notified += 1;
    });
    await mountPlugin(kernel, {
      id: "com.test.ui",
      apply: (ctx) => ctx.slots.registerUi({ slot: "toolbar/none", component: () => null }),
    });
    expect(notified).toBe(0);
    unsubscribe();
  });
});

describe("ctx.slots.declare / host（插件自声明槽位）", () => {
  it("A 声明 + host + B 贡献：list 生效；停用任一方各自撤销", async () => {
    kernel = createKernel();
    setPluginSlotHostComponent(() => null);
    await mountPlugin(kernel, {
      id: "com.timeline",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline/play", cardinality: "list", required: ["component"] });
        const Comp = ctx.slots.host("toolbar/timeline/play");
        expect(typeof Comp).toBe("function");
      },
    });
    await mountPlugin(kernel, {
      id: "com.timeline.contrib",
      apply: (ctx) => {
        ctx.slots.registerUi({ slot: "toolbar/timeline/play", component: () => null });
      },
    });
    expect(listSlot("toolbar/timeline/play")).toHaveLength(1);
    // 停用贡献方：贡献撤销，声明仍在（findSlotDeclarationRuntime 命中）。
    await unmountPlugin(kernel, "com.timeline.contrib");
    expect(listSlot("toolbar/timeline/play")).toEqual([]);
    expect(findSlotDeclarationRuntime("toolbar/timeline/play")).toBeDefined();
    // 停用声明方：声明撤销。
    await unmountPlugin(kernel, "com.timeline");
    expect(findSlotDeclarationRuntime("toolbar/timeline/play")).toBeUndefined();
  });

  it("停用声明方后他插件再向该槽贡献 → 该行 failed（贡献无处归属可见，不静默）", async () => {
    kernel = createKernel();
    await mountPlugin(kernel, {
      id: "com.timeline",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline/play", cardinality: "list", required: ["component"] });
      },
    });
    await unmountPlugin(kernel, "com.timeline");
    const result = await mountPlugin(kernel, {
      id: "com.timeline.contrib",
      apply: (ctx) => {
        ctx.slots.registerUi({ slot: "toolbar/timeline/play", component: () => null });
      },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("未声明的槽位");
    expect(listSlot("toolbar/timeline/play")).toEqual([]);
  });

  it("declare 冲突指名占用者：B 声明 A 已占 key → B failed 且消息含 A 的插件 id", async () => {
    kernel = createKernel();
    await mountPlugin(kernel, {
      id: "com.timeline",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline/play", cardinality: "list", required: ["component"] });
      },
    });
    const result = await mountPlugin(kernel, {
      id: "com.other",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline/play", cardinality: "list", required: ["component"] });
      },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("已被插件");
      expect(result.message).toContain("com.timeline");
    }
  });

  it("declare 宿主槽位受保护：宿主固定槽 / 宿主开放前缀均 failed", async () => {
    kernel = createKernel();
    for (const key of ["toolbar/note/right", "view/custom"]) {
      const result = await mountPlugin(kernel, {
        id: "com.timeline",
        apply: (ctx) => {
          ctx.slots.declare({ key, cardinality: "list", required: ["component"] });
        },
      });
      expect(result.ok, key).toBe(false);
      if (!result.ok) expect(result.message, key).toContain("宿主槽位");
    }
  });

  it("前缀声明：toolbar/timeline 前缀下任意槽可被贡献", async () => {
    kernel = createKernel();
    await mountPlugin(kernel, {
      id: "com.timeline",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline", prefix: true, cardinality: "list", required: ["component"] });
      },
    });
    await mountPlugin(kernel, {
      id: "com.timeline.contrib",
      apply: (ctx) => {
        ctx.slots.registerUi({ slot: "toolbar/timeline/play", component: () => null });
        ctx.slots.registerUi({ slot: "toolbar/timeline/export", component: () => null });
      },
    });
    expect(listSlot("toolbar/timeline/play")).toHaveLength(1);
    expect(listSlot("toolbar/timeline/export")).toHaveLength(1);
  });

  it("list()：合并视图含插件运行时声明（先到先得的槽可被发现）", async () => {
    kernel = createKernel();
    let seen: readonly SlotDeclaration[] = [];
    await mountPlugin(kernel, {
      id: "com.timeline",
      apply: (ctx) => {
        ctx.slots.declare({ key: "toolbar/timeline/play", cardinality: "list", required: ["component"] });
        seen = ctx.slots.list();
      },
    });
    expect(seen.some((d) => d.key === "toolbar/timeline/play")).toBe(true);
    expect(seen.some((d) => d.key === "toolbar/note/right")).toBe(true);
    expect(Object.isFrozen(seen)).toBe(true);
  });

  it("host() 托管未声明的槽 → 该行 failed（拼写错误可见，不静默渲染空白）", async () => {
    kernel = createKernel();
    const result = await mountPlugin(kernel, {
      id: "com.test.ui",
      apply: (ctx) => {
        ctx.slots.host("toolbar/none");
      },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("未声明的槽位");
  });
});
