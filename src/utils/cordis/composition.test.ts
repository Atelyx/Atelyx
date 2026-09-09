/**
 * 组合 profile 纯函数测试（utils/cordis/composition）。
 */
import { describe, expect, it } from "vitest";
import { resolveProfileMounts, type Profile } from "./composition";

const profile: Profile = {
  name: "default",
  plugins: [
    { id: "builtin.search", order: 1, defaultEnabled: true },
    { id: "builtin.recent", order: 2, defaultEnabled: true },
    { id: "builtin.calendar", order: 3, defaultEnabled: true },
    { id: "builtin.canvas", order: 5, defaultEnabled: true },
    { id: "builtin.table", order: 7, defaultEnabled: true },
    { id: "builtin.theme", order: 12, defaultEnabled: true, defaultConfig: { accent: true } },
  ],
};

describe("组合 profile", () => {
  it("按启用集合过滤 + profile 顺序返回挂载行", () => {
    const mounts = resolveProfileMounts(profile, new Set(["builtin.canvas", "builtin.search"]));
    expect(mounts.map((m) => m.id)).toEqual(["builtin.search", "builtin.canvas"]);
  });

  it("启用集合为空 → 无挂载行", () => {
    expect(resolveProfileMounts(profile, new Set())).toEqual([]);
  });

  it("附带默认 config（缺省 undefined）", () => {
    const mounts = resolveProfileMounts(profile, new Set(["builtin.canvas", "builtin.theme"]));
    expect(mounts.find((m) => m.id === "builtin.theme")?.config).toEqual({ accent: true });
    expect(mounts.find((m) => m.id === "builtin.canvas")?.config).toBeUndefined();
  });

  it("未在 profile 中的 id 不产生挂载行", () => {
    const mounts = resolveProfileMounts(profile, new Set(["com.third.party"]));
    expect(mounts).toEqual([]);
  });
});
