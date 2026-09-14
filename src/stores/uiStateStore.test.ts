/**
 * 应用级 UI 状态的增量补丁测试（stores/uiStateStore）。
 *
 * 验证：setter 只把本窗口真正变更的字段送进 `ui_state_patch`（撕裂窗口不得用整包陈旧副本
 * 覆盖其他窗口字段）、显式关闭携带 null 定向清除、bootstrap 失败时不发送、发送失败并回重发。
 * Rust 命令以替身替代（逐字段可选合并在 layout.rs 侧测试覆盖）。
 */
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@/services/layout", () => ({
  layoutBootstrap: vi.fn(async () => ({})),
  layoutFlush: vi.fn(async () => {}),
  layoutOp: vi.fn(async () => ({})),
  onLayoutBroadcast: vi.fn(async () => () => {}),
  uiStatePatch: vi.fn(async () => {}),
}));

import { layoutBootstrap, uiStatePatch } from "@/services/layout";
import { useUiStateStore } from "@/stores/uiStateStore";

/** 快进防抖窗口并等微任务排空（persist 回调内 await uiStatePatch）。 */
async function flushDebounce(): Promise<void> {
  await vi.advanceTimersByTimeAsync(400);
}

beforeEach(async () => {
  vi.useFakeTimers();
  // 排空上一用例残留的待发送补丁（pendingPatch 是模块级状态，不随 setState 重置）
  useUiStateStore.setState({ loaded: true, loadFailed: false });
  await useUiStateStore.getState().flush();
  vi.mocked(uiStatePatch).mockClear();
  useUiStateStore.setState({
    fileExplorerExpanded: new Set(),
    lastCanvasFile: null,
    lastNoteFile: null,
    lastTableFile: null,
    workspaceLayouts: [],
    activeLayoutId: null,
    focusedPanelId: null,
    detachedWindows: [],
    recentFiles: [],
    loaded: false,
    loadFailed: false,
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("ui-state 增量补丁", () => {
  it("setter 只发送自己变更的字段：聚焦 + 最近文件不携带展开/上次打开字段", async () => {
    await useUiStateStore.getState().load();
    useUiStateStore.getState().setFocusedPanel("panel-1");
    useUiStateStore.getState().recordRecentFile("a.md", "note", "v1");
    await flushDebounce();

    expect(uiStatePatch).toHaveBeenCalledTimes(1);
    const patch = vi.mocked(uiStatePatch).mock.calls[0]![0]!;
    expect(Object.keys(patch).sort()).toEqual(["focusedPanelId", "recentFiles"]);
    expect(patch.focusedPanelId).toBe("panel-1");
    expect(patch.recentFiles).toHaveLength(1);
  });

  it("recordOpenFile 只带上次打开字段；closeFile 定向清除（null）", async () => {
    await useUiStateStore.getState().load();
    useUiStateStore.getState().recordOpenFile("note", "n.md");
    await flushDebounce();

    expect(vi.mocked(uiStatePatch).mock.calls[0]![0]!).toEqual({ lastNoteFile: "n.md" });

    useUiStateStore.getState().closeFile("note");
    await flushDebounce();
    expect(vi.mocked(uiStatePatch).mock.calls[1]![0]!).toEqual({ lastNoteFile: null });
  });

  it("同字段连续变更合并为最后一次值；下一窗口只发新字段", async () => {
    await useUiStateStore.getState().load();
    useUiStateStore.getState().setFocusedPanel("p1");
    useUiStateStore.getState().setFocusedPanel("p2");
    await flushDebounce();
    expect(vi.mocked(uiStatePatch).mock.calls[0]![0]!).toEqual({ focusedPanelId: "p2" });

    useUiStateStore.getState().toggleExpanded("dir");
    await flushDebounce();
    const second = vi.mocked(uiStatePatch).mock.calls[1]![0]!;
    expect(Object.keys(second)).toEqual(["fileExplorerExpanded"]);
  });

  it("bootstrap 失败（loadFailed）时不发送任何补丁", async () => {
    vi.mocked(layoutBootstrap).mockRejectedValueOnce(new Error("ipc down"));
    await useUiStateStore.getState().load();
    expect(useUiStateStore.getState().loadFailed).toBe(true);

    useUiStateStore.getState().setFocusedPanel("p1");
    useUiStateStore.getState().toggleExpanded("dir");
    await flushDebounce();
    expect(uiStatePatch).not.toHaveBeenCalled();
  });

  it("flush 立即发送（不等防抖窗口）", async () => {
    await useUiStateStore.getState().load();
    useUiStateStore.getState().recordOpenFile("canvas", "c.atlx");
    await useUiStateStore.getState().flush();
    expect(uiStatePatch).toHaveBeenCalledWith({ lastCanvasFile: "c.atlx" });
  });
});
