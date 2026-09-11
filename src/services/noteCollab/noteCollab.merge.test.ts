/**
 * 基线 seed 幂等契约测试（services/noteCollab/notePeer.ts 的 `baselineSeedUpdate`）。
 *
 * 协议层（基线标签/采纳/三方合并）的收敛行为由 `notePeer.convergence.test.ts` 覆盖；
 * 本文件只锁定 CRDT 地基：**同一正文的确定性 seed 在任意端字节一致、合并幂等**
 * （各端以种子重建基线时不会产生同一文本的多份 item）。
 * 该性质是「异正文基线永不互相合并」之外的互补面：同正文基线可安全增量合并。
 */
import { describe, it, expect } from "vitest";
import * as Y from "yjs";
import { baselineSeedUpdate } from "./noteDoc";

const textOf = (doc: Y.Doc) => doc.getText("text").toString();
const countOf = (text: string, needle: string) => text.split(needle).length - 1;

/** 以确定性 seed 建文档，并给真实各端分配互不相同的客户端 id。 */
function peerDoc(text: string, clientId: number): Y.Doc {
  const doc = new Y.Doc();
  Y.applyUpdate(doc, baselineSeedUpdate(text));
  doc.clientID = clientId;
  return doc;
}

describe("baselineSeedUpdate", () => {
  it("空文档应用一次即得原文", () => {
    const doc = new Y.Doc();
    Y.applyUpdate(doc, baselineSeedUpdate("你好世界 abc123"));
    expect(textOf(doc)).toBe("你好世界 abc123");
  });

  it("同文本应用两次不翻倍", () => {
    const doc = new Y.Doc();
    Y.applyUpdate(doc, baselineSeedUpdate("abc"));
    Y.applyUpdate(doc, baselineSeedUpdate("abc"));
    expect(textOf(doc)).toBe("abc");
  });

  it("同一正文生成的 seed 字节一致（确定性）", () => {
    const body = "一段确定性文本 seed。";
    expect(baselineSeedUpdate(body)).toEqual(baselineSeedUpdate(body));
  });

  it("同正文基线两端互换全量状态：幂等不翻倍", () => {
    const body = "D0 text. A-edited B-edited ";
    const a = peerDoc(body, 123);
    const b = peerDoc(body, 456);
    Y.applyUpdate(b, Y.encodeStateAsUpdate(a));
    Y.applyUpdate(a, Y.encodeStateAsUpdate(b));
    expect(textOf(a)).toBe(body);
    expect(textOf(b)).toBe(body);
    expect(countOf(textOf(a), "A-edited")).toBe(1);
  });

  it("并发编辑在同一基线上正常收敛（非重叠双方保留）", () => {
    const a = peerDoc("D0 text. ", 123);
    const b = peerDoc("D0 text. ", 456);
    a.getText("text").insert(9, "A-edited ");
    b.getText("text").insert(9, "B-edited ");
    Y.applyUpdate(b, Y.encodeStateAsUpdate(a));
    Y.applyUpdate(a, Y.encodeStateAsUpdate(b));
    const merged = textOf(a);
    expect(textOf(b)).toBe(merged);
    expect(countOf(merged, "A-edited ")).toBe(1);
    expect(countOf(merged, "B-edited ")).toBe(1);
  });

  it("并发插入与删除在同一基线上按 CRDT 语义收敛", () => {
    const a = peerDoc("hello", 123);
    const b = peerDoc("hello", 456);
    a.getText("text").insert(2, "XX");
    b.getText("text").delete(0, 2);
    Y.applyUpdate(b, Y.encodeStateAsUpdate(a));
    Y.applyUpdate(a, Y.encodeStateAsUpdate(b));
    expect(textOf(a)).toBe(textOf(b));
  });

  it("异文本正文标识不同（异基线据此拒绝合并）", () => {
    expect(baselineSeedUpdate("甲版本")).not.toEqual(baselineSeedUpdate("乙版本"));
  });
});
