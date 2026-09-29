// @vitest-environment jsdom
/**
 * 插件详情弹窗声明能力标注测试（components/plugins/PluginDetailsDialog）。
 *
 * 纯 UI 逻辑：声明的 shell 在无进程执行平台标注「本平台不可用」。其余区块（审计/槽位/
 * 命令）不在本文件覆盖范围。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import type { InstalledPlugin, PluginSlotChain } from "@/types";
import { PluginDetailsDialog } from "./PluginDetailsDialog";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function plugin(over: {
  declares?: string[];
  phase?: InstalledPlugin["phase"];
} = {}): InstalledPlugin {
  return {
    id: "com.test.fs",
    manifest: {
      id: "com.test.fs",
      name: "FS 插件",
      version: "1.0.0",
      type: "background",
      ...(over.declares ? { declares: over.declares } : {}),
    },
    installDir: "/tmp/fs",
    sourceKind: "git",
    enabled: false,
    phase: over.phase ?? "active",
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

function mountDialog(p: InstalledPlugin, shellAvailable = true): HTMLDivElement {
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
        shellAvailable,
        getSlotChain: (slot: string): PluginSlotChain => ({
          slot,
          declarer: "宿主",
          contributors: [],
          decorators: [],
        }),
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

describe("PluginDetailsDialog 声明能力标注", () => {
  it("本平台无进程执行：声明的 shell 标注本平台不可用", () => {
    const el = mountDialog(plugin({ declares: ["shell"] }), false);
    expect(el.textContent).toContain("shell");
    expect(el.textContent).toContain("本平台不可用");
  });

  it("本平台有进程执行：声明的 shell 不加不可用标注", () => {
    const el = mountDialog(plugin({ declares: ["shell"] }), true);
    expect(el.textContent).toContain("shell");
    expect(el.textContent).not.toContain("本平台不可用");
  });
});
