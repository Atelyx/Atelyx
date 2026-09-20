// @vitest-environment jsdom
/**
 * 插件详情弹窗「外部目录访问」区测试（components/plugins/PluginDetailsDialog）。
 *
 * 纯 UI 逻辑：声明目录渲染 + 批准/撤销回调、已批准标记、清单更新后仍在批准的目录
 * 显式标注（可撤销）。其余区块（审计/槽位/命令）不在本文件覆盖范围。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import type { InstalledPlugin, PluginSlotChain } from "@/types";
import { PluginDetailsDialog } from "./PluginDetailsDialog";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const onApproveDir = vi.fn();
const onRevokeDir = vi.fn();

function plugin(over: {
  declaredDirs?: string[];
  approvedDirs?: string[];
  phase?: InstalledPlugin["phase"];
} = {}): InstalledPlugin {
  return {
    id: "com.test.fs",
    manifest: {
      id: "com.test.fs",
      name: "FS 插件",
      version: "1.0.0",
      type: "background",
      ...(over.declaredDirs ? { declaredDirs: over.declaredDirs } : {}),
    },
    installDir: "/tmp/fs",
    sourceKind: "git",
    enabled: false,
    phase: over.phase ?? "active",
    ...(over.approvedDirs ? { approvedDirs: over.approvedDirs } : {}),
  };
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(async () => {
  if (root) {
    await act(async () => {
      root!.unmount();
    });
    root = null;
  }
  if (container) {
    container.remove();
    container = null;
  }
});

function mountDialog(p: InstalledPlugin): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(
      React.createElement(PluginDetailsDialog, {
        plugin: p,
        commands: [],
        capabilityLabel: (n: string) => n,
        capabilitySensitive: () => false,
        getSlotChain: (slot: string): PluginSlotChain => ({
          slot,
          declarer: "宿主",
          contributors: [],
          decorators: [],
        }),
        onApproveDir,
        onRevokeDir,
        onRunCommand: () => {},
        onRollback: () => {},
        onClose: () => {},
        rollbackConfirm: false,
        onConfirmRollback: () => {},
        onCancelRollback: () => {},
      }),
    );
  });
  return container;
}

describe("PluginDetailsDialog 外部目录访问区", () => {
  it("无声明无批准 = 不渲染该区", () => {
    const el = mountDialog(plugin());
    expect(el.textContent).not.toContain("外部目录访问");
  });

  it("声明目录渲染：未批准显示批准按钮，点击回调 onApproveDir", () => {
    const el = mountDialog(plugin({ declaredDirs: ["~/Projects/foo", "/abs/path"] }));
    expect(el.textContent).toContain("外部目录访问");
    expect(el.textContent).toContain("~/Projects/foo");
    expect(el.textContent).toContain("/abs/path");
    const buttons = [...el.querySelectorAll("button")].filter((b) => b.textContent === "批准");
    expect(buttons).toHaveLength(2);
    act(() => {
      buttons[0]!.click();
    });
    expect(onApproveDir).toHaveBeenCalledWith("~/Projects/foo");
  });

  it("已批准目录显示撤销，点击回调 onRevokeDir", () => {
    const el = mountDialog(plugin({ declaredDirs: ["~/Projects/foo"], approvedDirs: ["~/Projects/foo"] }));
    const revoke = [...el.querySelectorAll("button")].filter((b) => b.textContent === "撤销");
    expect(revoke).toHaveLength(1);
    act(() => {
      revoke[0]!.click();
    });
    expect(onRevokeDir).toHaveBeenCalledWith("~/Projects/foo");
  });

  it("清单更新后仍在批准的目录（不再声明）显式标注且可撤销", () => {
    const el = mountDialog(plugin({ approvedDirs: ["~/Projects/old"] }));
    expect(el.textContent).toContain("~/Projects/old");
    expect(el.textContent).toContain("清单已不再声明");
    const revoke = [...el.querySelectorAll("button")].filter((b) => b.textContent === "撤销");
    expect(revoke).toHaveLength(1);
    act(() => {
      revoke[0]!.click();
    });
    expect(onRevokeDir).toHaveBeenCalledWith("~/Projects/old");
  });

  it("混合态：声明未批准/已批准/清单不再声明三态并存时按钮分配正确", () => {
    const el = mountDialog(
      plugin({
        declaredDirs: ["~/Projects/pending", "~/Projects/approved"],
        approvedDirs: ["~/Projects/approved", "~/Projects/ghost"],
      }),
    );
    expect(el.textContent).toContain("~/Projects/pending");
    expect(el.textContent).toContain("~/Projects/approved");
    expect(el.textContent).toContain("~/Projects/ghost");
    expect(el.textContent).toContain("清单已不再声明");
    const approve = [...el.querySelectorAll("button")].filter((b) => b.textContent === "批准");
    const revoke = [...el.querySelectorAll("button")].filter((b) => b.textContent === "撤销");
    // 仅未批准的声明目录可批准；已批准（含不再声明）可撤销
    expect(approve).toHaveLength(1);
    expect(revoke).toHaveLength(2);
    act(() => {
      approve[0]!.click();
      revoke[0]!.click();
      revoke[1]!.click();
    });
    expect(onApproveDir).toHaveBeenCalledWith("~/Projects/pending");
    expect(onRevokeDir).toHaveBeenCalledWith("~/Projects/approved");
    expect(onRevokeDir).toHaveBeenCalledWith("~/Projects/ghost");
  });

  it("重复声明的目录去重渲染（清单允许重复，UI 按唯一目录授权）", () => {
    const el = mountDialog(
      plugin({
        declaredDirs: ["~/Projects/dup", "~/Projects/dup"],
        approvedDirs: ["~/Projects/dup"],
      }),
    );
    const revoke = [...el.querySelectorAll("button")].filter((b) => b.textContent === "撤销");
    expect(revoke).toHaveLength(1);
  });
});
