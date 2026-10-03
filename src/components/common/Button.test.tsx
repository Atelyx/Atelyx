// @vitest-environment jsdom
/**
 * 图标按钮的 ref 透传与无障碍属性。
 *
 * 关键约束：`IconButton` 内部用 `Tooltip` 包一层提示，而提示靠 cloneElement 合并
 * ref/事件——若直接覆盖子元素 ref，调用方（如浮层按触发器 rect 定位）会拿到 null，
 * 表现为弹层锚到视口左上角。这类问题类型检查发现不了，必须由测试兜住。
 */
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { createRef } from "react";
import { render, cleanup, fireEvent, act } from "@testing-library/react";
import { Trash2 } from "lucide-react";
import { IconButton, Button } from "@/components/common/Button";

/**
 * jsdom 不做布局，所有元素的 rect恒为 0；而提示锚点与浮层定位都以 rect 为准
 * （rect 为 0 视为「触发器不可见」而不展示），故须给出非零 rect 才能测到提示。
 */
beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 100,
    y: 200,
    left: 100,
    top: 200,
    right: 120,
    bottom: 220,
    width: 20,
    height: 20,
    toJSON: () => ({}),
  } as DOMRect);
});

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
});

describe("IconButton", () => {
  it("forwardRef 落到 DOM 按钮上（浮层定位依赖触发器 rect）", () => {
    const ref = createRef<HTMLButtonElement>();
    render(<IconButton ref={ref} icon={<Trash2 size={12} />} label="删除" />);
    expect(ref.current).toBeInstanceOf(HTMLButtonElement);
    // rect 非零：提示锚点与浮层定位都据此计算
    expect(ref.current?.getBoundingClientRect().width).toBeGreaterThan(0);
  });

  it("图标按钮带可访问名，且不以原生 title 作提示", () => {
    const { getByRole } = render(<IconButton icon={<Trash2 size={12} />} label="删除条目" />);
    const btn = getByRole("button", { name: "删除条目" });
    // 原生 title 会与浮层提示同时出现（双提示），故不设
    expect(btn.getAttribute("title")).toBeNull();
  });

  it("禁用时不响应点击", () => {
    const onClick = vi.fn();
    const { getByRole } = render(
      <IconButton icon={<Trash2 size={12} />} label="删除" disabled onClick={onClick} />,
    );
    fireEvent.click(getByRole("button"));
    expect(onClick).not.toHaveBeenCalled();
  });

  it("保留调用方传入的 className 与行内色", () => {
    const { getByRole } = render(
      <IconButton
        icon={<Trash2 size={12} />}
        label="删除"
        className="shrink-0"
        style={{ color: "var(--accent)" }}
      />,
    );
    const btn = getByRole("button");
    expect(btn.className).toContain("shrink-0");
    expect(btn.style.color).toBe("var(--accent)");
  });

  it("悬停后浮出提示，移开后收起", () => {
    vi.useFakeTimers();
    try {
      const { getByRole } = render(<IconButton icon={<Trash2 size={12} />} label="删除文件" />);
      const btn = getByRole("button");
      expect(document.body.textContent).not.toContain("删除文件");
      // 提示有出现延迟：立即断言不出现，避免把「延迟」误测成「不生效」
      act(() => {
        fireEvent.mouseEnter(btn);
        vi.advanceTimersByTime(600);
      });
      expect(document.body.textContent).toContain("删除文件");
      act(() => {
        fireEvent.mouseLeave(btn);
        vi.advanceTimersByTime(300);
      });
      expect(document.body.textContent).not.toContain("删除文件");
    } finally {
      vi.useRealTimers();
    }
  });

  /**
   * `noTooltip` 必须连提示一起关掉：提示的层级（z-1200）高于它所遮挡的浮层
   * （如文件树触屏菜单 z-50），且触屏无 `mouseleave` 收不掉。只断言类名会漏掉
   * 「提示仍然弹出」的实现回归，故这里走真实悬停。
   */
  it("noTooltip 时悬停不浮出提示（避免压住同时打开的浮层）", () => {
    vi.useFakeTimers();
    try {
      const { getByRole } = render(
        <IconButton icon={<Trash2 size={12} />} label="文件操作" noTooltip />,
      );
      act(() => {
        fireEvent.mouseEnter(getByRole("button"));
        vi.advanceTimersByTime(600);
      });
      expect(document.body.textContent).not.toContain("文件操作");
    } finally {
      vi.useRealTimers();
    }
  });

  it("透传 aria-* 状态属性（如展开键的 aria-expanded）", () => {
    const { getByRole, rerender } = render(
      <IconButton icon={<Trash2 size={12} />} label="版本详情" aria-expanded={false} />,
    );
    expect(getByRole("button").getAttribute("aria-expanded")).toBe("false");
    rerender(<IconButton icon={<Trash2 size={12} />} label="版本详情" aria-expanded />);
    expect(getByRole("button").getAttribute("aria-expanded")).toBe("true");
  });
});

/**
 * 变体的悬停反馈必须能盖过内联底色。
 *
 * 底色/文字色经内联 `style` 给出（供主题插件与调用方动态覆盖），而内联样式在层叠中
 * 优先于无 `!important` 的作者规则——若 hover 类不带 `!important`，悬停反馈会被静默吃掉，
 * 表现为「鼠标移上去没反应」。jsdom 不实现 `:hover`，故此处直接断言产物类名带 `!`。
 */
describe("Button 悬停反馈", () => {
  const VARIANTS = ["primary", "secondary", "ghost", "subtle", "danger", "dangerSolid"] as const;

  it.each(VARIANTS)("%s 变体的 hover 类均带 !important", (variant) => {
    const { getByRole } = render(<Button variant={variant}>操作</Button>);
    const cls = getByRole("button").className;
    const hoverClasses = cls.split(/\s+/).filter((c) => c.startsWith("hover:"));
    expect(hoverClasses.length).toBeGreaterThan(0);
    expect(hoverClasses.every((c) => c.includes("hover:!"))).toBe(true);
  });

  it("IconButton 默认 ghost 变体同样带 !important 悬停底色", () => {
    const { getByRole } = render(<IconButton icon={<Trash2 size={12} />} label="删除" />);
    const cls = getByRole("button").className;
    expect(cls).toContain("hover:!bg-[var(--hover)]");
  });
});
