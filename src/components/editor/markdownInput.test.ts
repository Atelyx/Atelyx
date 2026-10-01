/**
 * 列表行 Enter 文本变换单测：延续项标记 / 空项退出列表 / 非列表行走默认。
 */
import { describe, expect, it } from "vitest";
import { listEnterEdit } from "./markdownInput";

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

  it("空项 Enter 退出列表：整行标记清掉，光标到行首", () => {
    const text = "- 甲\n- ";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "- 甲\n", cursor: 4 });
  });

  it("缩进空项退出后不留缩进", () => {
    const text = "  - ";
    const edit = listEnterEdit(text, text.length);
    expect(edit).toEqual({ text: "", cursor: 0 });
  });

  it("非列表标记行返回 null（走默认换行）", () => {
    expect(listEnterEdit("普通段落", 4)).toBeNull();
    // 松散项的续行（无标记）不延续列表
    expect(listEnterEdit("- 甲\n续行", 7)).toBeNull();
  });
});
