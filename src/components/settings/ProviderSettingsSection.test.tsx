// @vitest-environment jsdom
/**
 * 模型供应商面板的 key 落点与空间提示：
 * 本地仓库显示「API key 随仓库保存」开关；协作空间不显示该开关（key 由服务端团队元数据承载）、
 * 显示团队共享说明。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ProviderSettingsSection } from "@/components/settings/ProviderSettingsSection";

const { flags, EMPTY_PROVIDERS, NO_SYNC_KEYS } = vi.hoisted(() => ({
  flags: { isSpace: false, viewerOnly: false },
  // 选择器必须返回稳定引用：zustand 以快照比较，每次新建数组/对象会触发无限重渲染
  EMPTY_PROVIDERS: [] as unknown[],
  NO_SYNC_KEYS: { syncKeys: false } as Record<string, unknown>,
}));

vi.mock("@/hooks/useEditingTarget", () => ({
  useEditingTargetFlags: () => flags,
}));


vi.mock("@/stores/settingsStore", async () => {
  const { create } = await import("zustand");
  const useSettingsStore = create(() => ({
    addProvider: vi.fn(),
    updateProvider: vi.fn(),
    removeProvider: vi.fn(),
    setSyncKeys: vi.fn(),
  }));
  return {
    useSettingsStore,
    selectEditingProviders: () => EMPTY_PROVIDERS,
    selectEditingVaultConfig: () => NO_SYNC_KEYS,
  };
});

beforeEach(() => {
  cleanup();
  flags.isSpace = false;
  flags.viewerOnly = false;
});

describe("模型供应商面板的 key 落点", () => {
  it("本地仓库：显示「API key 随仓库保存」开关，不显示空间说明", () => {
    render(<ProviderSettingsSection />);
    expect(screen.getByText("API key 随仓库保存")).toBeTruthy();
    expect(screen.queryByText(/空间的模型供应商与 API key/)).toBeNull();
    expect(screen.queryByText("从本机旧配置导入")).toBeNull();
  });

  it("协作空间：不显示开关，显示团队共享说明", () => {
    flags.isSpace = true;
    render(<ProviderSettingsSection />);
    expect(screen.queryByText("API key 随仓库保存")).toBeNull();
    expect(screen.getByText(/空间的模型供应商与 API key 由所有者或编辑者统一配置/)).toBeTruthy();
  });

  it("协作空间 + 查看者：提示角色（写入口由 store 侧拒绝）", () => {
    flags.isSpace = true;
    flags.viewerOnly = true;
    render(<ProviderSettingsSection />);
    expect(screen.getByText(/你在该空间内是查看者/)).toBeTruthy();
  });
});
