// @vitest-environment jsdom
/**
 * 插件详情弹窗能力面展示测试（components/plugins/PluginDetailsDialog）。
 *
 * 纯 UI 逻辑：宿主自发现的能力 chips（含 shell 平台不可用标注）与无记录时的提示文案。
 * 其余区块（失败诊断/槽位/命令）不在本文件覆盖范围。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import type { InstalledPlugin, PluginAuditEntry, PluginSlotChain } from "@/types";
import { PluginDetailsDialog } from "./PluginDetailsDialog";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function plugin(over: { type?: InstalledPlugin["manifest"]["type"] } = {}): InstalledPlugin {
  return {
    id: "com.test.fs",
    manifest: {
      id: "com.test.fs",
      name: "FS 插件",
      version: "1.0.0",
      type: over.type ?? "background",
    },
    installDir: "/tmp/fs",
    sourceKind: "git",
    enabled: false,
    phase: "active",
  };
}

function audit(over: Partial<PluginAuditEntry> = {}): PluginAuditEntry {
  return {
    pluginId: "com.test.fs",
    services: [],
    events: [],
    calls: [],
    slotContributions: [],
    slotDecorators: [],
    ...over,
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

function mountDialog(p: InstalledPlugin, entry?: PluginAuditEntry, shellAvailable = true): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(
      React.createElement(PluginDetailsDialog, {
        plugin: p,
        audit: entry,
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

describe("PluginDetailsDialog 能力面（宿主自发现）", () => {
  it("有审计记录：展示自发现的服务 chips（服务读与调用摘要去重合并）", () => {
    const el = mountDialog(
      plugin(),
      audit({ services: ["vault"], calls: [{ service: "shell", method: "exec", summary: "cmd /c build（3 个参数）" }] }),
    );
    expect(el.textContent).toContain("能力面");
    expect(el.textContent).toContain("vault");
    expect(el.textContent).toContain("shell");
  });

  it("本平台无进程执行：发现到的 shell 标注本平台不可用", () => {
    const el = mountDialog(plugin(), audit({ services: ["shell"] }), false);
    expect(el.textContent).toContain("shell");
    expect(el.textContent).toContain("本平台不可用");
  });

  it("本平台有进程执行：发现到的 shell 不加不可用标注", () => {
    const el = mountDialog(plugin(), audit({ services: ["shell"] }), true);
    expect(el.textContent).toContain("shell");
    expect(el.textContent).not.toContain("本平台不可用");
  });

  it("无审计记录：提示能力面由宿主运行时自动发现", () => {
    const el = mountDialog(plugin());
    expect(el.textContent).toContain("能力面");
    expect(el.textContent).toContain("无需开发者声明");
  });

  it("纯 theme 插件（声明式皮肤，无运行时访问）：不渲染能力面区", () => {
    const el = mountDialog(plugin({ type: "theme" }));
    expect(el.textContent).not.toContain("能力面");
  });
});
