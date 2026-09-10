/**
 * 应用内通知运行时测试（stores/notificationStore）。
 * 只覆盖列表行为：push 返回 id、dismiss 幂等；自动消失计时归宿主组件。
 */
import { afterEach, describe, expect, it } from "vitest";
import { useNotificationStore } from "./notificationStore";

afterEach(() => {
  for (const item of useNotificationStore.getState().items) {
    useNotificationStore.getState().dismiss(item.id);
  }
});

describe("notificationStore", () => {
  it("notify 追加并返回唯一 id（含级别与自动消失时长）", () => {
    const first = useNotificationStore.getState().notify({ message: "已保存" });
    const second = useNotificationStore.getState().notify({ message: "失败", level: "error", title: "错误" });
    expect(first).not.toBe(second);
    const items = useNotificationStore.getState().items;
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ id: first, level: "info", message: "已保存" });
    expect(items[1]).toMatchObject({ id: second, level: "error", title: "错误" });
    expect(items[1]!.timeoutMs).toBeGreaterThan(items[0]!.timeoutMs);
  });

  it("dismiss 移除指定项，未知 id 不改动列表", () => {
    const id = useNotificationStore.getState().notify({ message: "x" });
    const before = useNotificationStore.getState().items;
    useNotificationStore.getState().dismiss("no-such-id");
    expect(useNotificationStore.getState().items).toBe(before);
    useNotificationStore.getState().dismiss(id);
    expect(useNotificationStore.getState().items).toEqual([]);
  });
});
