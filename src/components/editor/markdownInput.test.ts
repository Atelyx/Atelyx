// @vitest-environment jsdom
/**
 * 输入面单测：列表行 Enter/退格、Tab 缩进/反缩进的文本变换，输入法组合态（未上屏文本 + 选区折算），
 * 以及输入面基态样式的承载方式。
 */
import { describe, expect, it } from "vitest";
import { CaretOverlay, indentEdit, listBackspaceEdit, listEnterEdit, MarkdownEditSink, outdentEdit, type CompositionText, type RemoteCursor } from "./markdownInput";

/** 组装输入面；`onCompositionChange` 记录每次组合态上报。 */
function mountSink(value: string, caret: number) {
  const host = document.createElement("div");
  const reports: (CompositionText | null)[] = [];
  const sink = new MarkdownEditSink(host, {
    onTextChange: () => {},
    onSelectionChange: () => {},
    onCompositionChange: (_composing, preedit) => reports.push(preedit),
  });
  sink.setText(value);
  sink.el.setSelectionRange(caret, caret);
  return { sink, reports };
}

/** jsdom 未实现 CompositionEvent：用普通事件携带 data 字段。 */
function emitComposition(el: HTMLTextAreaElement, type: string, data: string): void {
  const event = new Event(type);
  Object.defineProperty(event, "data", { value: data });
  el.dispatchEvent(event);
}

describe("输入法组合态", () => {
  it("组合期间上报未上屏文本及其正文起点", () => {
    const { sink, reports } = mountSink("甲乙", 1);
    emitComposition(sink.el, "compositionstart", "");
    expect(sink.preedit).toEqual({ at: 1, text: "", remove: 0 });
    sink.el.value = "甲ni乙"; // 输入面 value 组合期已含未上屏文本
    emitComposition(sink.el, "compositionupdate", "ni");
    expect(sink.preedit).toEqual({ at: 1, text: "ni", remove: 0 });
    expect(reports.at(-1)).toEqual({ at: 1, text: "ni", remove: 0 });
  });

  it("组合替换选区：上报被替换正文长度（编辑面据此不再渲染旧文字）", () => {
    const { sink } = mountSink("甲乙丙", 1);
    sink.el.setSelectionRange(1, 3); // 选中「乙丙」后起组合
    emitComposition(sink.el, "compositionstart", "");
    sink.el.value = "甲ni";
    emitComposition(sink.el, "compositionupdate", "ni");
    expect(sink.preedit).toEqual({ at: 1, text: "ni", remove: 2 });
  });

  it("组合期选区折算回正文坐标（光标不被未上屏文本推走）", () => {
    const { sink } = mountSink("甲乙", 1);
    emitComposition(sink.el, "compositionstart", "");
    sink.el.value = "甲ni乙";
    emitComposition(sink.el, "compositionupdate", "ni");
    sink.el.setSelectionRange(3, 3); // 光标在未上屏文本之后（value 坐标）
    expect(sink.selection).toEqual({ from: 1, to: 1 });
  });

  it("组合替换选区时按被替换长度补回正文坐标", () => {
    const { sink } = mountSink("甲乙丙", 1);
    sink.el.setSelectionRange(1, 2); // 选中「乙」后起组合
    emitComposition(sink.el, "compositionstart", "乙");
    sink.el.value = "甲ni丙";
    emitComposition(sink.el, "compositionupdate", "ni");
    sink.el.setSelectionRange(3, 3);
    expect(sink.selection).toEqual({ from: 2, to: 2 });
    emitComposition(sink.el, "compositionend", "ni");
    expect(sink.preedit).toBeNull();
  });
});

describe("listEnterEdit", () => {
  it("无序项尾 Enter 延续同字符标记", () => {
    const text = "- 甲";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "- 甲\n- ", cursor: text.length + 3 });
  });

  it("有序项序号 +1 且保留定界符", () => {
    const text = "2) 甲";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "2) 甲\n3) ", cursor: text.length + 4 });
  });

  it("任务项延续为未完成任务", () => {
    const text = "- [x] 已完成";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "- [x] 已完成\n- [ ] ", cursor: text.length + 7 });
  });

  it("嵌套项保持缩进（同级新项）", () => {
    const text = "  - 甲";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "  - 甲\n  - ", cursor: text.length + 5 });
  });

  it("光标在行中 Enter 拆项：后半文本成为新项内容", () => {
    const edit = listEnterEdit("- 甲乙", 3); // 「甲|乙」之间
    expect(edit).toEqual({ text: "- 甲\n- 乙", cursor: 6 });
  });

  it("空项 Enter 继续列表：新开一个同级空项，光标落在其上", () => {
    const text = "- 甲\n- ";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "- 甲\n- \n- ", cursor: 9 });
  });

  it("缩进空项 Enter 同样延续，缩进保持同级", () => {
    const text = "  - ";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "  - \n  - ", cursor: 9 });
  });

  it("非列表标记行返回 null（走默认换行）", () => {
    expect(listEnterEdit("普通段落", 4)).toBeNull();
    // 松散项的续行（无标记）不延续列表
    expect(listEnterEdit("- 甲\n续行", 7)).toBeNull();
    // 光标停在标记之前/之中：在标记前换行，续标记会拆出嵌套列表
    expect(listEnterEdit("- 甲", 0)).toBeNull();
    expect(listEnterEdit("- 甲", 1)).toBeNull();
    expect(listEnterEdit("  1. 甲", 3)).toBeNull();
  });
});

describe("listBackspaceEdit", () => {
  it("空项标记之后退格：整段标记一次删掉，留下一行空行、光标到行首", () => {
    expect(listBackspaceEdit("- 甲\n- ", 6)).toEqual({ text: "- 甲\n", cursor: 4 });
    expect(listBackspaceEdit("  - ", 4)).toEqual({ text: "", cursor: 0 });
    expect(listBackspaceEdit("- [ ] ", 6)).toEqual({ text: "", cursor: 0 });
    expect(listBackspaceEdit("1. ", 3)).toEqual({ text: "", cursor: 0 });
  });

  it("有内容的项、光标在行首：返回 null（逐字符退格 / 并进上一行交浏览器）", () => {
    expect(listBackspaceEdit("- 甲", 3)).toBeNull();
    expect(listBackspaceEdit("普通段落", 4)).toBeNull();
    // 行首：这一下该并进上一行，由浏览器原生完成
    expect(listBackspaceEdit("- ", 0)).toBeNull();
    // 标记之内（`-` 与空格之间）同样算「删这个空项」
    expect(listBackspaceEdit("- ", 1)).toEqual({ text: "", cursor: 0 });
    // 标记后无空格的行（`-` 独占一行）同样在标记之内/之后触发
    expect(listBackspaceEdit("-", 1)).toEqual({ text: "", cursor: 0 });
  });
});

describe("indentEdit", () => {
  it("列表行整行缩进：光标在行中任意位置都右移，且光标随之偏移", () => {
    expect(indentEdit("- 甲", 3, 3)).toEqual({ text: "  - 甲", from: 5, to: 5 });
    // 光标停在行首也整行缩进（光标落到缩进之后）
    expect(indentEdit("- 甲", 0, 0)).toEqual({ text: "  - 甲", from: 2, to: 2 });
    // 有序项、任务项同为列表行
    expect(indentEdit("1. 甲", 4, 4)).toEqual({ text: "  1. 甲", from: 6, to: 6 });
    expect(indentEdit("- [ ] 甲", 7, 7)).toEqual({ text: "  - [ ] 甲", from: 9, to: 9 });
  });

  it("嵌套场景：光标停在第二项上按 Tab，只有该项成为上一项的子项", () => {
    const text = "- 项目一\n- 项目二";
    expect(indentEdit(text, text.length, text.length)).toEqual({ text: "- 项目一\n  - 项目二", from: 13, to: 13 });
  });

  it("非列表行只在光标处插入缩进（代码块/段落行内对齐用）", () => {
    expect(indentEdit("普通段落", 2, 2)).toEqual({ text: "普通  段落", from: 4, to: 4 });
    expect(indentEdit("const x = 1;", 12, 12)).toEqual({ text: "const x = 1;  ", from: 14, to: 14 });
    expect(indentEdit("普通段落", 0, 0)).toEqual({ text: "  普通段落", from: 2, to: 2 });
  });

  it("有选区：覆盖到的每一行整行缩进，选区仍罩住原内容", () => {
    const text = "- 甲\n- 乙\n- 丙";
    // 选区终点落在第三行行首：只缩进前两行
    expect(indentEdit(text, 2, 8)).toEqual({ text: "  - 甲\n  - 乙\n- 丙", from: 4, to: 12 });
    // 单行内的部分选区同样按整行处理
    expect(indentEdit("普通段落", 1, 3)).toEqual({ text: "  普通段落", from: 3, to: 5 });
  });
});

describe("outdentEdit", () => {
  it("按一级缩进左移：空格最多删一个单位、行首制表符整体删一个", () => {
    expect(outdentEdit("  - 甲", 4, 4)).toEqual({ text: "- 甲", from: 2, to: 2 });
    expect(outdentEdit(" - 甲", 3, 3)).toEqual({ text: "- 甲", from: 2, to: 2 });
    expect(outdentEdit("\t- 甲", 4, 4)).toEqual({ text: "- 甲", from: 3, to: 3 });
    // 缩进不足一个单位：整段删掉、光标回到行首
    expect(outdentEdit("  - 甲", 1, 1)).toEqual({ text: "- 甲", from: 0, to: 0 });
  });

  it("多行选区：逐行左移，已顶格的行不动", () => {
    const text = "  - 甲\n- 乙\n  - 丙";
    expect(outdentEdit(text, 2, text.length)).toEqual({ text: "- 甲\n- 乙\n- 丙", from: 0, to: 11 });
  });

  it("无可删缩进返回 null", () => {
    expect(outdentEdit("- 甲", 3, 3)).toBeNull();
    expect(outdentEdit("普通段落", 2, 2)).toBeNull();
  });
});

describe("隐藏输入面基态样式", () => {
  it("基态走类名而非 style 属性：打包后自定义 scheme 下 style 属性的样式不参与层叠", () => {
    const { sink } = mountSink("甲", 1);
    expect(sink.el.className).toBe("md-edit-sink");
    // 基态声明一条都不在 style 属性里（style 属性只有运行期覆写：内部高度与光标坐标）
    for (const prop of ["opacity", "width", "position", "z-index", "resize"]) {
      expect(sink.el.style.getPropertyValue(prop)).toBe("");
    }
  });

  it("动态坐标与内部高度经 CSSOM 覆写类中的基态值", () => {
    const { sink } = mountSink("甲乙丙", 1);
    sink.moveTo({ left: 208, top: 20 });
    sink.syncScroll();
    expect(sink.el.style.left).toBe("208px");
    expect(sink.el.style.top).toBe("20px");
    // 内部高度按行高实测覆写类中的 1em 基态
    expect(sink.el.style.height).toMatch(/^\d+(\.\d+)?px$/);
  });
});

/** jsdom 无布局：用平面矩形对象充当量测结果。 */
function fakeRect(left: number, top: number, width = 0, height = 0): DOMRect {
  return { left, top, width, height, right: left + width, bottom: top + height, x: left, y: top } as DOMRect;
}

describe("协作者光标与选区绘制", () => {
  it("选区矩形铺协作者色半透明底，光标竖线带昵称标签（坐标相对绘制层原点）", () => {
    const host = document.createElement("div");
    const overlay = new CaretOverlay(host);
    const cursors: RemoteCursor[] = [
      {
        rects: [fakeRect(110, 220, 40, 20)],
        rect: fakeRect(150, 220, 0, 20),
        label: "甲",
        color: "#ff0000",
      },
    ];
    overlay.setRemoteCursors(cursors, fakeRect(100, 200));
    const layer = host.querySelector(".md-remote-layer")!;
    const boxes = layer.querySelectorAll<HTMLElement>(".md-remote-selection-rect");
    expect(boxes).toHaveLength(1);
    expect(boxes[0]!.style.left).toBe("10px");
    expect(boxes[0]!.style.top).toBe("20px");
    expect(boxes[0]!.style.width).toBe("40px");
    expect(boxes[0]!.style.height).toBe("20px");
    // 底色按协作者用户色调低浓度（与本地选区同浓度），色值经 CSSOM 内联
    expect(boxes[0]!.style.background).toContain("color-mix");
    expect(boxes[0]!.style.background).toContain("rgb(255, 0, 0)");
    const caret = layer.querySelector<HTMLElement>(".md-remote-caret")!;
    expect(caret.style.left).toBe("50px");
    expect(caret.style.background).toBe("rgb(255, 0, 0)");
    expect(caret.querySelector(".md-remote-caret-label")!.textContent).toBe("甲");
  });

  it("折叠光标不画选区；量不出光标矩形时只画选区", () => {
    const host = document.createElement("div");
    const overlay = new CaretOverlay(host);
    overlay.setRemoteCursors(
      [
        { rects: [], rect: fakeRect(10, 10, 0, 18), label: "", color: "#00ff00" },
        { rects: [fakeRect(0, 0, 30, 18)], rect: null, label: "乙", color: "#0000ff" },
      ],
      fakeRect(0, 0),
    );
    const layer = host.querySelector(".md-remote-layer")!;
    expect(layer.querySelectorAll(".md-remote-selection-rect")).toHaveLength(1);
    expect(layer.querySelectorAll(".md-remote-caret")).toHaveLength(1);
    // 无昵称不出标签
    expect(layer.querySelector(".md-remote-caret .md-remote-caret-label")).toBeNull();
  });
});
