/**
 * 插件市场数据源纯逻辑测试（services/plugins/market）。
 *
 * 覆盖：官方账号判定、徽标合并（官方按 owner、精选严格按 owner/repo，自报 id 不授予）、缓存过期判定。
 * fetch/缓存读写（网络 + localStorage）不在本测试覆盖。
 */
import { describe, it, expect } from "vitest";
import { badgeFor, isMarketStale, isOfficialRepo } from "./market";

describe("market 纯逻辑", () => {
  it("官方账号判定", () => {
    expect(isOfficialRepo("Xuhang944/plugins")).toBe(true);
    expect(isOfficialRepo("someone/plugins")).toBe(false);
  });

  it("徽标：官方账号 → official；精选严格按 owner/repo，自报 id 不继承（防伪造）", () => {
    const endorsed = new Set(["nice/repo"]);
    expect(badgeFor({ repo: "Xuhang944/x", id: "a" }, endorsed)).toBe("official");
    expect(badgeFor({ repo: "nice/repo", id: "b" }, endorsed)).toBe("endorsed");
    // 同 id 不同作者仓库（仿冒克隆）：不继承精选——id + 作者账号双重校验。
    expect(badgeFor({ repo: "other/repo", id: "com.good.plugin" }, endorsed)).toBeUndefined();
    expect(badgeFor({ repo: "other/repo", id: "c" }, endorsed)).toBeUndefined();
    // 精选表中只有 id 而无 owner/repo（历史/误填）：同样不授予。
    expect(badgeFor({ repo: "some/repo", id: "com.good.plugin" }, new Set(["com.good.plugin"]))).toBeUndefined();
  });

  it("缓存过期判定（6h 窗口）", () => {
    expect(isMarketStale(0)).toBe(true);
    expect(isMarketStale(Date.now())).toBe(false);
    expect(isMarketStale(Date.now() - 7 * 60 * 60 * 1000)).toBe(true);
  });
});
