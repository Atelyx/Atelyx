/**
 * 插件工具注册 → Agent 名册接线测试（services/ai/tools）：插件工具注册后进入执行名册与 UI 元数据（「插件」分类）、可被 buildAgentTools 组装、注销后从两处移除。
 * 注册/注销保持平衡，不污染其他测试。
 */
import { describe, it, expect } from "vitest";
import {
  buildAgentTools,
  pluginToolMetas,
  registerPluginTools,
  unregisterPluginTools,
} from "./index";
import type { ToolDefinition } from "@/types";

const pluginTool = (name: string): ToolDefinition => ({
  name,
  description: `插件工具 ${name}`,
  parameters: {},
  validate: (args) => args as Record<string, unknown>,
  summarize: () => name,
  execute: async () => ({ ok: true, summary: name }),
});

describe("插件工具注册", () => {
  it("注册后进入名册与 UI 元数据，可被 buildAgentTools 组装", () => {
    const tool = pluginTool("my_tool");
    registerPluginTools([tool]);
    try {
      const meta = pluginToolMetas().find((m) => m.id === "my_tool");
      expect(meta?.category).toBe("plugin");
      expect(buildAgentTools(["my_tool"], true).tools.map((t) => t.name)).toContain("my_tool");
    } finally {
      unregisterPluginTools([tool]);
    }
  });

  it("注销后从名册与元数据移除", () => {
    const tool = pluginTool("gone_tool");
    registerPluginTools([tool]);
    unregisterPluginTools([tool]);
    expect(pluginToolMetas().map((m) => m.id)).not.toContain("gone_tool");
    expect(buildAgentTools(["gone_tool"], true).tools).toEqual([]);
  });
});
