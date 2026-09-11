/**
 * 笔记协作接线测试（services/noteCollab/noteDoc.ts）：以假广播钩子驱动模块级单例，
 * 锁定 store 依赖的接线契约——绑定即通告基线并握手、正文差量同步、落盘登记推进共同祖先、
 * 入站帧路由与重建回调、目录前缀销毁、重连反熵只覆盖激活文档。
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import {
  applyLocalBody,
  bindNoteDoc,
  destroyAllNoteDocs,
  disposeNoteDocsUnder,
  markNoteDiskWrite,
  receiveSyncMessage,
  resyncAllNoteDocs,
  setNoteCollabBindingRefresh,
  setNoteCollabBroadcast,
  unbindNoteDoc,
} from "./noteDoc";
import { baselineIdOf, decodeNoteFrame, encodeNoteBaseline } from "./frame";

const FILE = "notes/a.md";
const IDENTITY = { name: "甲", color: "#123456" };

interface SentFrame {
  file: string;
  kind: string;
  tag: { seq: number; author: string; id: string };
  currentText?: string;
}

let frames: SentFrame[];
let awareness: Array<{ file: string; bytes: number }>;
let rebuilds: Array<{ file: string; text: string }>;

function decodeSent(file: string, payload: Uint8Array): SentFrame {
  const frame = decodeNoteFrame(payload);
  expect(frame).not.toBeNull();
  if (frame?.kind === "baseline") {
    return { file, kind: "baseline", tag: frame.tag, currentText: frame.currentText };
  }
  if (frame?.kind === "sync") return { file, kind: "sync", tag: frame.tag };
  return { file, kind: "resync", tag: { seq: 0, author: "", id: "" } };
}

beforeEach(() => {
  destroyAllNoteDocs();
  frames = [];
  awareness = [];
  rebuilds = [];
  setNoteCollabBroadcast({
    sendSyncMessage: (file, payload) => frames.push(decodeSent(file, payload)),
    sendAwareness: (file, payload) => awareness.push({ file, bytes: payload.length }),
  });
  setNoteCollabBindingRefresh((file, doc) => {
    rebuilds.push({ file, text: doc.ytext.toString() });
  });
});

afterEach(() => {
  setNoteCollabBindingRefresh(null);
  setNoteCollabBroadcast(null);
  destroyAllNoteDocs();
});

/** 构造对端通告帧（基线正文 + 当前正文）。 */
function peerBaseline(
  seq: number,
  author: string,
  baselineText: string,
  currentText: string,
): Uint8Array {
  return encodeNoteBaseline(
    { seq, author, id: baselineIdOf(baselineText) },
    baselineText,
    currentText,
  );
}

describe("绑定与出站", () => {
  it("绑定即通告本地基线与握手（标签与基线正文一致）", () => {
    const doc = bindNoteDoc(FILE, "一\n二\n", IDENTITY);
    expect(doc.ytext.toString()).toBe("一\n二\n");
    expect(frames.map((f) => f.kind)).toEqual(["baseline", "sync"]);
    const baseline = frames[0];
    expect(baseline.tag.id).toBe(baselineIdOf("一\n二\n"));
    expect(baseline.currentText).toBe("一\n二\n");
    expect(frames[1].tag).toEqual(baseline.tag);
  });

  it("正文同步：相同正文不发帧，不同正文按差量发一帧", () => {
    bindNoteDoc(FILE, "一\n二\n", IDENTITY);
    const before = frames.length;
    applyLocalBody(FILE, "一\n二\n");
    expect(frames.length).toBe(before);
    applyLocalBody(FILE, "一甲\n二\n");
    expect(frames.length).toBe(before + 1);
    expect(frames[frames.length - 1].kind).toBe("sync");
  });

  it("未绑定文件的正文同步/落盘登记为 no-op（不抛异常）", () => {
    expect(() => applyLocalBody("notes/none.md", "x")).not.toThrow();
    expect(() => markNoteDiskWrite("notes/none.md", "x")).not.toThrow();
    expect(() => unbindNoteDoc("notes/none.md")).not.toThrow();
  });
});

describe("入站路由与重建", () => {
  it("更高序对端通告：合并后重建文档并刷新绑定", () => {
    bindNoteDoc(FILE, "第一段\n第二段\n", IDENTITY);
    const peerSeq = 9;
    receiveSyncMessage(
      FILE,
      peerBaseline(peerSeq, "对端", "第一段\n第二段\n新增段\n", "第一段\n第二段\n新增段\n"),
      1,
    );
    expect(rebuilds).toHaveLength(1);
    expect(rebuilds[0].file).toBe(FILE);
    expect(rebuilds[0].text).toBe("第一段\n第二段\n新增段\n");
  });

  it("同基线正文的通告：判兼容、不重建（对端改动经同步帧到达）", () => {
    const doc = bindNoteDoc(FILE, "第一段\n第二段\n", IDENTITY);
    applyLocalBody(FILE, "第一段甲改\n第二段\n");
    markNoteDiskWrite(FILE, "第一段甲改\n第二段\n");
    // 基线正文与本端一致（字节同一 seed）→ 只判兼容，不走采纳重建；其改动应由同步帧合并
    receiveSyncMessage(FILE, peerBaseline(5, "对端", "第一段\n第二段\n", "第一段\n第二段\n对端新增\n"), 1);
    expect(rebuilds).toHaveLength(0);
    expect(doc.ytext.toString()).toBe("第一段甲改\n第二段\n");
  });

  it("坏帧与异基线同步帧被安全忽略", () => {
    bindNoteDoc(FILE, "内容\n", IDENTITY);
    expect(() => receiveSyncMessage(FILE, new Uint8Array([0x42]), 1)).not.toThrow();
    expect(() => receiveSyncMessage(FILE, new Uint8Array([]), 1)).not.toThrow();
    expect(rebuilds).toHaveLength(0);
  });
});

describe("生命周期", () => {
  it("重连反熵只为激活文档发握手（解绑后不再发）", () => {
    bindNoteDoc(FILE, "内容\n", IDENTITY);
    const active = frames.length;
    resyncAllNoteDocs();
    expect(frames.length).toBe(active + 1);
    unbindNoteDoc(FILE);
    const released = frames.length;
    resyncAllNoteDocs();
    expect(frames.length).toBe(released);
  });

  it("目录前缀销毁后同路径再绑定得到新文档", () => {
    const first = bindNoteDoc("dir/a.md", "旧内容\n", IDENTITY);
    bindNoteDoc("dir/sub/b.md", "更深一层\n", IDENTITY);
    disposeNoteDocsUnder("dir");
    const again = bindNoteDoc("dir/a.md", "同路径新文件\n", IDENTITY);
    expect(again).not.toBe(first);
    expect(again.ytext.toString()).toBe("同路径新文件\n");
  });

  it("全部销毁后重新绑定得到新文档且重新通告基线", () => {
    const first = bindNoteDoc(FILE, "内容\n", IDENTITY);
    destroyAllNoteDocs();
    frames = [];
    const second = bindNoteDoc(FILE, "内容\n", IDENTITY);
    expect(second).not.toBe(first);
    expect(frames.map((f) => f.kind)).toEqual(["baseline", "sync"]);
  });
});
