/**
 * 插件启用依赖解析（requires 启动前校验）单测。
 * 覆盖——宿主命名空间满足、依赖链顺序、缺失报错、依赖成环报错、迭代上限兜底。
 */
import { describe, it, expect } from "vitest";
import { resolveEnabledDeps } from "./pluginDeps";

describe("resolveEnabledDeps", () => {
  it("无依赖：全部可启动（顺序不变）", () => {
    const r = resolveEnabledDeps(
      [
        { id: "a", provides: ["com.a"] },
        { id: "b" },
      ],
      [],
    );
    expect(r.spawnable).toEqual(["a", "b"]);
    expect(r.failed).toEqual([]);
  });

  it("宿主命名空间满足 requires；提供者先于依赖者", () => {
    const r = resolveEnabledDeps(
      [
        { id: "dep", requires: ["shell"], provides: ["com.dep"] },
        { id: "user", requires: ["com.dep"] },
      ],
      ["shell", "state"],
    );
    expect(r.spawnable).toEqual(["dep", "user"]);
    expect(r.failed).toEqual([]);
  });

  it("依赖缺失：拒绝并给出可读原因", () => {
    const r = resolveEnabledDeps([{ id: "user", requires: ["com.missing"] }], ["shell"]);
    expect(r.spawnable).toEqual([]);
    expect(r.failed).toEqual([{ id: "user", reason: "依赖能力 com.missing 缺失（提供者未安装/未启用）" }]);
  });

  it("依赖成环：双方失败且报成环原因", () => {
    const r = resolveEnabledDeps(
      [
        { id: "a", provides: ["com.a"], requires: ["com.b"] },
        { id: "b", provides: ["com.b"], requires: ["com.a"] },
      ],
      [],
    );
    expect(r.spawnable).toEqual([]);
    expect(r.failed).toHaveLength(2);
    for (const f of r.failed) expect(f.reason).toContain("依赖成环");
  });

  it("环内夹真缺失：按缺失报（不误判为成环）", () => {
    const r = resolveEnabledDeps(
      [
        { id: "a", provides: ["com.a"], requires: ["com.b"] },
        { id: "b", provides: ["com.b"], requires: ["com.a", "com.gone"] },
      ],
      [],
    );
    const byId = Object.fromEntries(r.failed.map((f) => [f.id, f.reason]));
    expect(byId["a"]).toContain("依赖成环"); // a 缺 b，b 在残留环内
    expect(byId["b"]).toContain("com.gone 缺失"); // b 夹真缺失，不误判为成环
  });

  it("依赖链超过一轮解析：提供者在第二轮满足依赖者", () => {
    const r = resolveEnabledDeps(
      [
        { id: "a", provides: ["com.a"] },
        { id: "b", provides: ["com.b"], requires: ["com.a"] },
        { id: "c", requires: ["com.b"] },
      ],
      [],
    );
    expect(r.spawnable).toEqual(["a", "b", "c"]);
    expect(r.failed).toEqual([]);
  });

  it("4 层依赖链：固定点迭代直至收敛（无误报）", () => {
    const r = resolveEnabledDeps(
      [
        { id: "a", provides: ["com.a"] },
        { id: "b", provides: ["com.b"], requires: ["com.a"] },
        { id: "c", provides: ["com.c"], requires: ["com.b"] },
        { id: "d", requires: ["com.c"] },
      ],
      [],
    );
    expect(r.spawnable).toEqual(["a", "b", "c", "d"]);
    expect(r.failed).toEqual([]);
  });

  it("畸形清单守卫：requires/provides 非数组按空处理，不拖垮解析", () => {
    // 前端拿到的原始清单（Rust 侧仅校验 declares），畸形 requires/provides 不应让整个加载崩溃
    const r = resolveEnabledDeps(
      [
        {
          id: "a",
          provides: "com.a" as unknown as string[],
          requires: "com.x" as unknown as string[],
        },
      ],
      [],
    );
    expect(r.spawnable).toEqual(["a"]);
    expect(r.failed).toEqual([]);
  });
});
