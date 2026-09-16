/**
 * AI 对话核心能力注册表契约测试：注册/取用/撤销与订阅通知。
 *
 * 锁定两条消费方依赖的性质：未注册 = 能力不可用（消费者据此降级）、
 * 撤销按引用守卫（旧提供者的撤销不得摘掉新提供者注册的能力）。
 */
import { describe, expect, it, vi } from "vitest";
import type { ChatRuntime } from "@/types";
import { getChatRuntime, onChatRuntimeChange, registerChatRuntime } from "./chatRuntimeHost";

function stub(): ChatRuntime {
  return {
    resolveTarget: () => ({ ok: false, reason: "no-model", error: "未配置" }),
    runTurn: async () => {},
    compact: async () => ({ ok: false, aborted: false, message: "未启用" }),
    autoName: async () => "skipped",
  };
}

describe("AI 对话核心能力注册表", () => {
  it("未注册 = 能力不可用；注册后取到同一对象；撤销后回到不可用", () => {
    expect(getChatRuntime()).toBeNull();
    const runtime = stub();
    const off = registerChatRuntime(runtime);
    expect(getChatRuntime()).toBe(runtime);
    off();
    expect(getChatRuntime()).toBeNull();
  });

  it("后注册者生效，旧提供者的撤销按引用守卫（不摘掉新提供者）", () => {
    const first = stub();
    const second = stub();
    const offFirst = registerChatRuntime(first);
    const offSecond = registerChatRuntime(second);
    expect(getChatRuntime()).toBe(second);
    offFirst();
    expect(getChatRuntime()).toBe(second);
    offSecond();
    expect(getChatRuntime()).toBeNull();
  });

  it("注册/撤销通知订阅方（可用性变化驱动消费者重读）", () => {
    const listener = vi.fn();
    const off = onChatRuntimeChange(listener);
    const stop = registerChatRuntime(stub());
    expect(listener).toHaveBeenCalledTimes(1);
    stop();
    expect(listener).toHaveBeenCalledTimes(2);
    off();
    registerChatRuntime(stub())();
    expect(listener).toHaveBeenCalledTimes(2);
  });
});
