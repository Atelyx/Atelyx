/**
 * 笔记协作帧编解码测试（services/noteCollab/frame.ts）：三类帧往返、标签全序、
 * 坏帧（空/截断/未知类型/尾随字节）一律返回 null 而不抛异常。
 */
import { describe, it, expect } from "vitest";
import * as encoding from "lib0/encoding";
import {
  baselineIdOf,
  compareTag,
  decodeNoteFrame,
  encodeNoteBaseline,
  encodeNoteResync,
  encodeNoteSync,
  sameContent,
  NOTE_FRAME_BASELINE,
  type BaselineTag,
} from "./frame";

const body = "一段基线正文\n第二行\n";
const tag: BaselineTag = { seq: 7, author: "设备A#ab12", id: baselineIdOf(body) };

describe("标签", () => {
  it("全序：seq → author → id，三段无平局（同 seq/author 不同正文也有确定胜负）", () => {
    expect(compareTag({ seq: 1, author: "b", id: "x" }, { seq: 2, author: "a", id: "y" })).toBe(-1);
    expect(compareTag({ seq: 2, author: "a", id: "x" }, { seq: 2, author: "b", id: "y" })).toBe(-1);
    expect(compareTag({ seq: 2, author: "b", id: "x" }, { seq: 2, author: "b", id: "y" })).toBe(-1);
    expect(compareTag({ seq: 2, author: "b", id: "y" }, { seq: 2, author: "b", id: "x" })).toBe(1);
    expect(compareTag({ seq: 2, author: "b", id: "x" }, { seq: 2, author: "b", id: "x" })).toBe(0);
    expect(compareTag({ seq: 3, author: "a", id: "x" }, { seq: 2, author: "z", id: "y" })).toBe(1);
  });

  it("内容标识只随正文变化：同正文不同 seq/author 判兼容", () => {
    const a: BaselineTag = { seq: 1, author: "甲", id: baselineIdOf(body) };
    const b: BaselineTag = { seq: 9, author: "乙", id: baselineIdOf(body) };
    expect(sameContent(a, b)).toBe(true);
    expect(compareTag(a, b)).not.toBe(0);
    expect(sameContent(a, { ...a, id: baselineIdOf(`${body}增补`) })).toBe(false);
  });

  it("内容标识对长度敏感（前缀/追加/替换都不等）", () => {
    const ids = new Set(
      ["", "a", "ab", "ba", "a\u0000b", "😀", "😀😀", "\uD83D"].map((t) => baselineIdOf(t)),
    );
    expect(ids.size).toBe(8);
  });

  it("相等判定即全序为 0：序号/身份/正文标识任一不同即不等", () => {
    expect(compareTag(tag, { ...tag })).toBe(0);
    expect(compareTag(tag, { ...tag, author: "别的" })).not.toBe(0);
    expect(compareTag(tag, { ...tag, id: "另一个" })).not.toBe(0);
  });
});

describe("帧往返", () => {
  it("同步帧：标签与内嵌载荷原样还原", () => {
    const inner = new Uint8Array([0, 1, 2, 250, 255]);
    const frame = decodeNoteFrame(encodeNoteSync(tag, inner));
    expect(frame).toEqual({ kind: "sync", tag, payload: inner });
  });

  it("基线通告帧：基线正文与当前正文分别还原（含空文本与超长文本）", () => {
    for (const body of ["", "一段正文\n带换行\n", "长文本".repeat(5000)]) {
      const frame = decodeNoteFrame(encodeNoteBaseline(tag, body, `${body}当前追加`));
      expect(frame).toEqual({
        kind: "baseline",
        tag,
        baselineText: body,
        currentText: `${body}当前追加`,
      });
    }
  });

  it("重同步帧", () => {
    expect(decodeNoteFrame(encodeNoteResync(0))).toEqual({ kind: "resync", reason: 0 });
    expect(decodeNoteFrame(encodeNoteResync(1))).toEqual({ kind: "resync", reason: 1 });
  });

  it("标签中的中文与特殊字符 author 往返一致", () => {
    const weird: BaselineTag = { seq: 4294967295, author: "张 三\t#1\u0000尾", id: "长-1-2" };
    const frame = decodeNoteFrame(encodeNoteBaseline(weird, "内容", "内容已编辑"));
    expect(frame).toEqual({
      kind: "baseline",
      tag: weird,
      baselineText: "内容",
      currentText: "内容已编辑",
    });
  });
});

describe("坏帧", () => {
  it("空载荷 → null", () => {
    expect(decodeNoteFrame(new Uint8Array([]))).toBeNull();
  });

  it("未知帧类型 → null", () => {
    const encoder = encoding.createEncoder();
    encoding.writeVarUint(encoder, 0x7f);
    expect(decodeNoteFrame(encoding.toUint8Array(encoder))).toBeNull();
  });

  it("截断的标签/文本 → null", () => {
    const full = encodeNoteBaseline(tag, "完整正文", "完整正文");
    expect(decodeNoteFrame(full.slice(0, full.length - 2))).toBeNull();
    const sync = encodeNoteSync(tag, new Uint8Array([1, 2, 3]));
    expect(decodeNoteFrame(sync.slice(0, 3))).toBeNull();
  });

  it("尾随多余字节 → null", () => {
    const buffer = new Uint8Array([...encodeNoteResync(0), 9]);
    expect(decodeNoteFrame(buffer)).toBeNull();
  });

  it("opcode 常量互不冲突且与 y-protocols 消息类型错开", () => {
    expect(new Set([NOTE_FRAME_BASELINE, 0, 1, 2]).size).toBe(4);
  });
});
