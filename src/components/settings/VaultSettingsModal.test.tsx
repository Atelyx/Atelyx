// @vitest-environment jsdom
/**
 * 仓库设置弹窗的会话三态：读取中 / 读取失败（可重试）/ 就绪，
 * 以及「编辑非激活仓库」横幅与「编辑激活仓库」时的标题口径。
 * 子面板与插件 store 以替身注入（本测试只覆盖弹窗壳的状态分派）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";

const { reloadMock } = vi.hoisted(() => ({ reloadMock: vi.fn() }));

/** 会话桩（null = 编辑激活仓库）。 */
const h = vi.hoisted(() => ({ session: null as unknown }));

vi.mock("@/stores/settingsStore", async () => {
  const { create } = await import("zustand");
  const useSettingsStore = create(() => ({
    reloadVaultSettingsSession: reloadMock,
  }));
  return {
    useSettingsStore,
    selectVaultSettingsSession: () => h.session,
  };
});

vi.mock("@/stores/pluginStore", async () => {
  const { create } = await import("zustand");
  const usePluginStore = create(() => ({ uiRevision: 0, pluginSettings: () => [] }));
  return { usePluginStore };
});

vi.mock("@/components/settings/ProviderSettingsSection", () => ({
  ProviderSettingsSection: () => <div>供应商面板</div>,
}));
vi.mock("@/components/settings/AgentSettingsSection", () => ({
  AgentSettingsSection: () => <div>Agent 面板</div>,
}));
vi.mock("@/components/settings/tabs/ModelServicesSettingsTab", () => ({
  ModelServicesSettingsTab: () => <div>模型服务面板</div>,
}));
vi.mock("@/components/settings/tabs/SearchSettingsTab", () => ({
  SearchSettingsTab: () => <div>联网搜索面板</div>,
}));
vi.mock("@/components/settings/tabs/FilesSettingsTab", () => ({
  FilesSettingsTab: () => <div>文件与路径面板</div>,
}));
vi.mock("@/components/settings/tabs/EditorSettingsTab", () => ({
  EditorSettingsTab: () => <div>编辑器面板</div>,
}));
vi.mock("@/components/settings/AboutSection", () => ({ AboutSection: () => <div>关于面板</div> }));
vi.mock("@/components/plugins/PluginsSettingsTab", () => ({
  PluginsSettingsTab: () => <div>插件面板</div>,
}));
vi.mock("@/components/settings/tabs/ThemeSettingsTab", () => ({
  ThemeSettingsTab: () => <div>主题面板</div>,
}));
vi.mock("@/components/settings/tabs/CollabSettingsTab", () => ({
  CollabSettingsTab: () => <div>协作面板</div>,
}));
vi.mock("@/components/settings/tabs/GeneralSettingsTab", () => ({
  GeneralSettingsTab: () => <div>通用面板</div>,
}));

const { VaultSettingsModal } = await import("@/components/settings/SettingsModal");

const TARGET = {
  kind: "local" as const,
  root: "E:/另一个仓库",
  name: "另一个仓库",
};

/** 会话桩：默认读取就绪。 */
function sessionOf(patch: Record<string, unknown>) {
  return {
    id: 1,
    target: TARGET,
    loaded: true,
    error: null,
    readOnly: false,
    corruptBackup: null,
    vaultConfig: {},
    config: { providers: [] },
    searchConfig: { provider: "tavily", searxngUrl: "" },
    tavilyKey: "",
    agents: [],
    promptNotes: [],
    ...patch,
  };
}

function renderModal() {
  return render(<VaultSettingsModal target={TARGET} onClose={vi.fn()} />);
}

beforeEach(() => {
  cleanup();
  h.session = null;
  reloadMock.mockReset();
});

describe("仓库设置弹窗", () => {
  it("编辑激活仓库（无会话）：不显示非当前仓库横幅，标题标注当前仓库", () => {
    renderModal();
    expect(screen.queryByText(/正在编辑非当前仓库/)).toBeNull();
    expect(screen.getByText(/当前仓库：另一个仓库/)).toBeTruthy();
    expect(screen.getByText("供应商面板")).toBeTruthy();
  });

  it("读取中：显示读取提示，不渲染设置内容", () => {
    h.session = sessionOf({ loaded: false, error: null });
    renderModal();
    expect(screen.getByText(/正在读取该仓库的配置/)).toBeTruthy();
    expect(screen.queryByText("供应商面板")).toBeNull();
  });

  it("读取失败：显示原因与重试入口，重试调会话重载", () => {
    h.session = sessionOf({ loaded: false, error: "仓库路径不可达" });
    renderModal();
    expect(screen.getByText(/该仓库的配置未能读取：仓库路径不可达/)).toBeTruthy();
    fireEvent.click(screen.getByText("重试"));
    expect(reloadMock).toHaveBeenCalledTimes(1);
  });

  it("编辑非激活仓库：横幅点名目标与身份；只读会话额外提示改动不会保存", () => {
    h.session = sessionOf({});
    renderModal();
    expect(screen.getByText(/正在编辑非当前仓库/)).toBeTruthy();
    expect(screen.getByText("另一个仓库")).toBeTruthy();
    // 目标身份（本地仓库路径）在标题与横幅里都出现，便于确认改的是哪个仓库
    expect(screen.getAllByText(/E:\/另一个仓库/).length).toBeGreaterThan(0);
    expect(screen.queryByText(/查看者/)).toBeNull();

    cleanup();
    h.session = sessionOf({ readOnly: true });
    renderModal();
    expect(screen.getByText(/你在该空间内是查看者/)).toBeTruthy();
  });
});
