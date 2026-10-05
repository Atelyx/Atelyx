/**
 * 插件浮层运行时契约测试（stores/floatingLayerStore.ts）：登记/收起/onClose 至多一次。
 */
import { describe, it, expect } from "vitest";
import { useFloatingLayerStore } from "./floatingLayerStore";

const entry = () => ({
  pluginId: "com.test.plugin",
  component: () => null,
  placement: "center" as const,
  closeOnOutsideClick: false,
});

describe("floatingLayerStore", () => {
  it("open 登记 + close 收起（onClose 触发一次）", () => {
    const store = useFloatingLayerStore.getState();
    let closed = 0;
    const id = store.open({ ...entry(), onClose: () => closed++ });
    expect(useFloatingLayerStore.getState().layers).toHaveLength(1);
    store.close(id);
    expect(useFloatingLayerStore.getState().layers).toHaveLength(0);
    expect(closed).toBe(1);
    // 重复收起 = no-op（onClose 不再触发）
    store.close(id);
    expect(closed).toBe(1);
  });

  it("close 不存在的 id = no-op", () => {
    useFloatingLayerStore.getState().close("missing");
    expect(useFloatingLayerStore.getState().layers).toHaveLength(0);
  });

  it("多层按登记顺序叠放", () => {
    const store = useFloatingLayerStore.getState();
    const a = store.open(entry());
    const b = store.open({ ...entry(), pluginId: "com.test.other" });
    expect(useFloatingLayerStore.getState().layers.map((l) => l.id)).toEqual([a, b]);
    store.close(a);
    expect(useFloatingLayerStore.getState().layers.map((l) => l.id)).toEqual([b]);
    useFloatingLayerStore.setState({ layers: [] });
  });
});
