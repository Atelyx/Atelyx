// @vitest-environment jsdom
/**
 * 能力降级门控 hook：激活仓库为协作空间 = true，个人仓库 / 未激活 = false。
 * appStore 以最小 zustand 替身注入，验证 hook 订阅身份变化。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { useIsSpaceVault } from "@/hooks/useIsSpaceVault";
import { useAppStore } from "@/stores/appStore";

vi.mock("@/stores/appStore", async () => {
  const { create } = await import("zustand");
  const useAppStore = create(() => ({
    vaultIdentity: null as null | { kind: "local"; root: string } | { kind: "space"; serverUrl: string; spaceId: string },
  }));
  return { useAppStore };
});

function Probe() {
  const isSpace = useIsSpaceVault();
  return <div data-testid="is-space">{String(isSpace)}</div>;
}

beforeEach(() => {
  cleanup();
  useAppStore.setState({ vaultIdentity: null });
});

describe("useIsSpaceVault 门控", () => {

it("协作空间身份 → true", () => {
  useAppStore.setState({ vaultIdentity: { kind: "space", serverUrl: "http://s1", spaceId: "sp1" } });
  render(<Probe />);
  expect(screen.getByTestId("is-space").textContent).toBe("true");
});

it("个人仓库身份 → false", () => {
  useAppStore.setState({ vaultIdentity: { kind: "local", root: "/tmp/vault" } });
  render(<Probe />);
  expect(screen.getByTestId("is-space").textContent).toBe("false");
});

it("未激活仓库 → false", () => {
  render(<Probe />);
  expect(screen.getByTestId("is-space").textContent).toBe("false");
});
});
