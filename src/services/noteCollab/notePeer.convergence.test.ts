/**
 * 笔记协作 peer 多端收敛测试（services/noteCollab/notePeer.ts）。
 *
 * 在单进程内驱动 2-3 个 peer（内存广播 + 逐帧投递），锁定四条不变量：
 * - 同正文基线的两端按普通 CRDT 增量合并（不重建、不提案，无重复、无丢失）；
 * - 异正文基线绝不互相合并：异基线同步帧被丢弃并回通告（防「尾部混入异文本」）；
 * - 迟到端/脏端采纳更高序基线时按三方合并，双方内容都保留；
 * - 重开复用文档（不回退、不重建、不重发基线通告），静默后不再有新帧（终止性）。
 */
import { describe, it, expect } from "vitest";
import * as Y from "yjs";
import { applyAwarenessUpdate, Awareness, encodeAwarenessUpdate } from "y-protocols/awareness";
import * as decoding from "lib0/decoding";
import {
  baselineIdOf,
  compareTag,
  decodeNoteFrame,
  encodeNoteBaseline,
  encodeNoteResync,
  type BaselineTag,
} from "./frame";
import { createNotePeer, type NoteIdentity, type NotePeer, type NoteRemoteAuthor } from "./notePeer";

const FILE = "notes/a.md";
const IDENTITY: NoteIdentity = { name: "用户", color: "#30bced" };

/** 投递步数上限：超过即视为未收敛（防实现缺陷把测试挂死）。 */
const DELIVER_LIMIT = 500;

interface InboxMessage {
  file: string;
  payload: Uint8Array;
  from: number;
}

interface NodeStats {
  rebuilds: number;
  baselinesSent: number;
  syncsSent: number;
  awarenessSent: number;
}

interface Node {
  id: number;
  peer: NotePeer;
  inbox: InboxMessage[];
  stats: NodeStats;
  /** 出站 awareness 载荷（断言节流合并时解码其 client 集合）。 */
  awarenessFrames: Uint8Array[];
}

interface Cluster {
  nodes: Node[];
  /** 投递全部在途帧直到静默，返回投递步数。 */
  deliver: () => number;
  text: (index: number, file?: string) => string;
  tag: (index: number) => BaselineTag | null;
  /** 丢弃全部在途帧（模拟 relay 广播裁剪或隔离某方向流量）。 */
  dropAll: () => void;
}

function makeCluster(size: number, authors?: string[]): Cluster {
  const nodes: Node[] = [];
  for (let i = 0; i < size; i++) {
    const stats: NodeStats = { rebuilds: 0, baselinesSent: 0, syncsSent: 0, awarenessSent: 0 };
    const inbox: InboxMessage[] = [];
    const awarenessFrames: Uint8Array[] = [];
    const peer = createNotePeer({
      author: authors?.[i] ?? `peer-${i}`,
      send: (file, payload) => {
        const frame = decodeNoteFrame(payload);
        if (frame?.kind === "baseline") stats.baselinesSent += 1;
        else if (frame?.kind === "sync") stats.syncsSent += 1;
        for (const other of nodes) {
          if (other.id === i) continue;
          other.inbox.push({ file, payload: payload.slice(), from: i });
        }
      },
      sendAwareness: (_file, payload) => {
        stats.awarenessSent += 1;
        awarenessFrames.push(payload);
      },
      onDocRebuilt: () => {
        stats.rebuilds += 1;
      },
    });
    nodes.push({ id: i, peer, inbox, stats, awarenessFrames });
  }

  function deliver(): number {
    let steps = 0;
    for (;;) {
      let progressed = false;
      for (const node of nodes) {
        while (node.inbox.length > 0) {
          const msg = node.inbox.shift()!;
          if (steps++ > DELIVER_LIMIT) {
            // 帧风暴/未收敛：所有调用点统一失败，避免只靠被断言的节点文本恰好自洽而掩盖
            throw new Error(`投递未收敛：超过 ${DELIVER_LIMIT} 帧`);
          }
          node.peer.receive(msg.file, msg.payload, msg.from, undefined);
          progressed = true;
        }
      }
      if (!progressed) return steps;
    }
  }

  return {
    nodes,
    deliver,
    text: (index, file = FILE) => nodes[index].peer.getText(file) ?? "",
    tag: (index) => nodes[index].peer.getTag(FILE),
    dropAll: () => {
      for (const node of nodes) node.inbox.length = 0;
    },
  };
}

const countOf = (text: string, needle: string) => text.split(needle).length - 1;

/** 解码 awareness 更新的 client id 集合（`varUint(count)` + 每项 `varUint(id) varUint(clock) varString(state)`）。 */
function awarenessClients(payload: Uint8Array): number[] {
  const decoder = decoding.createDecoder(payload);
  const count = decoding.readVarUint(decoder);
  const ids: number[] = [];
  for (let i = 0; i < count; i++) {
    ids.push(decoding.readVarUint(decoder));
    decoding.readVarUint(decoder); // clock
    decoding.readVarString(decoder); // state
  }
  return ids;
}

describe("同正文基线：并发输入按 CRDT 增量合并", () => {
  it("两端各自打开同一文本、各改一段：都不重建、无重复、结果一致", () => {
    const c = makeCluster(2);
    const base = "第一段\n第二段\n第三段\n";
    c.nodes[0].peer.open(FILE, base, IDENTITY);
    c.nodes[1].peer.open(FILE, base, IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "第一段甲改\n第二段\n第三段\n");
    c.nodes[1].peer.syncBody(FILE, "第一段\n第二段\n第三段乙改\n");
    c.deliver();
    const result = c.text(0);
    expect(c.text(1)).toBe(result);
    expect(result).toBe("第一段甲改\n第二段\n第三段乙改\n");
    expect(countOf(result, "甲改")).toBe(1);
    expect(countOf(result, "乙改")).toBe(1);
    // 同正文基线按增量合并：不重建文档、不发布新基线
    expect(c.nodes[0].stats.rebuilds).toBe(0);
    expect(c.nodes[1].stats.rebuilds).toBe(0);
  });

  it("三端并发各改一段：全部保留且完全一致", () => {
    const c = makeCluster(3);
    const base = "一\n二\n三\n四\n";
    for (const node of c.nodes) node.peer.open(FILE, base, IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "一甲\n二\n三\n四\n");
    c.nodes[1].peer.syncBody(FILE, "一\n二乙\n三\n四\n");
    c.nodes[2].peer.syncBody(FILE, "一\n二\n三丙\n四\n");
    c.deliver();
    const result = c.text(0);
    expect(c.text(1)).toBe(result);
    expect(c.text(2)).toBe(result);
    expect(result).toBe("一甲\n二乙\n三丙\n四\n");
  });

  it("静默后不再产生新帧（收敛终止）", () => {
    const c = makeCluster(2);
    c.nodes[0].peer.open(FILE, "内容\n", IDENTITY);
    c.nodes[1].peer.open(FILE, "内容\n", IDENTITY);
    c.deliver();
    expect(c.deliver()).toBe(0);
    c.nodes[0].peer.syncBody(FILE, "内容甲\n");
    c.deliver();
    expect(c.deliver()).toBe(0);
  });
});

describe("异正文基线：不合并、按序协商", () => {
  it("异基线同步帧被丢弃并回通告（不产生尾部混入）", () => {
    const c = makeCluster(2);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    a.open(FILE, "AAA 本地版本含尾部\n", IDENTITY);
    b.open(FILE, "BBB 对端", IDENTITY);
    c.dropAll();
    b.syncBody(FILE, "BBB 对端版本含额外尾部");
    const payload = c.nodes[0].inbox.at(-1)!.payload;
    c.dropAll();
    const before = c.text(0);
    const baselinesBefore = c.nodes[0].stats.baselinesSent;
    a.receive(FILE, payload, 1, undefined);
    expect(c.text(0)).toBe(before);
    expect(c.nodes[0].stats.baselinesSent).toBeGreaterThan(baselinesBefore);
  });

  it("序号更高的异正文基线胜出：低序号端整篇采纳并重建一次", () => {
    const c = makeCluster(2);
    c.nodes[0].peer.open(FILE, "甲版本\n", IDENTITY);
    // 乙在甲之后建立基线（序号 +1）：异正文按序号取高者，双方收敛到同正文同标签
    c.nodes[1].peer.open(FILE, "乙版本\n", IDENTITY);
    c.deliver();
    expect(c.text(0)).toBe("乙版本\n");
    expect(c.text(1)).toBe("乙版本\n");
    expect(c.nodes[0].stats.rebuilds).toBe(1);
    expect(compareTag(c.tag(0)!, c.tag(1)!)).toBe(0);
  });

  it("同基线对端编辑后再通告：仍判兼容，后续增量不被误拒", () => {
    const c = makeCluster(3);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    const x = c.nodes[2].peer;
    a.open(FILE, "一\n二\n", IDENTITY);
    b.open(FILE, "一\n二\n", IDENTITY);
    c.deliver();
    a.syncBody(FILE, "一甲\n二\n");
    c.deliver();
    c.dropAll();
    // 第三方异基线握手帧令甲回通告（通告带「基线正文 + 当前正文」，当前正文已非基线正文）
    x.open(FILE, "异正文\n", IDENTITY);
    const xHandshake = c.nodes[0].inbox.at(-1)!.payload;
    c.dropAll();
    a.receive(FILE, xHandshake, 2, undefined);
    const announcement = c.nodes[1].inbox.at(-1)!.payload;
    c.dropAll();
    const rebuildsBefore = c.nodes[1].stats.rebuilds;
    b.receive(FILE, announcement, 0, undefined);
    expect(c.nodes[1].stats.rebuilds).toBe(rebuildsBefore);
    // 同基线正文 → 不得被记成实证不兼容：其后的增量帧必须照常应用
    a.syncBody(FILE, "一甲乙\n二\n");
    const edit = c.nodes[1].inbox.at(-1)!.payload;
    c.dropAll();
    b.receive(FILE, edit, 0, undefined);
    expect(c.text(1)).toBe("一甲乙\n二\n");
  });

  it("落后对端以更高序号提案：按序号采纳其正文（异基线整篇提案的 LWW 口径，非损坏）", () => {
    const c = makeCluster(2);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    // 甲以「旧正文 + 自己的落盘改动」建立基线并落盘
    a.open(FILE, "一\n二\n", IDENTITY);
    a.syncBody(FILE, "一甲改\n二\n");
    a.markDiskWrite(FILE, "一甲改\n二\n");
    c.dropAll();
    // 乙从另一份更旧的正文起家（异基线），序号更高 → 按序采纳其正文
    b.open(FILE, "一\n二\n三\n", IDENTITY);
    b.syncBody(FILE, "一\n二\n三\n对端新增\n");
    c.deliver();
    expect(c.text(0)).toBe(c.text(1));
    expect(c.text(0)).toContain("对端新增");
    // 两侧基线不同且无共同祖先判据：按序号让位（LWW），不是内容损坏
    expect(countOf(c.text(0), "对端新增")).toBe(1);
  });

  it("迟到端带外部新增内容：双方内容都保留（三方合并）", () => {
    const c = makeCluster(2);
    c.nodes[0].peer.open(FILE, "第一段\n第二段\n", IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "第一段甲改\n第二段\n");
    // 乙打开时磁盘已多出一段（外部改动），甲未见过
    c.nodes[1].peer.open(FILE, "第一段\n第二段\n新增段\n", IDENTITY);
    c.deliver();
    const result = c.text(0);
    expect(c.text(1)).toBe(result);
    expect(result).toBe("第一段甲改\n第二段\n新增段\n");
  });

  it("本地有未落盘输入时采纳更高序基线：输入与对端内容都保留", () => {
    const c = makeCluster(2);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    a.open(FILE, "开头\n结尾\n", IDENTITY);
    c.deliver();
    a.syncBody(FILE, "开头甲新增\n结尾\n");
    b.open(FILE, "开头\n结尾\n乙段\n", IDENTITY);
    c.deliver();
    const result = c.text(0);
    expect(c.text(1)).toBe(result);
    expect(result).toBe("开头甲新增\n结尾\n乙段\n");
  });
});

describe("重开与保留文档", () => {
  it("重开（会话关闭再打开，磁盘正文未变）不重建、不回退、不重发基线通告", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "初始\n", IDENTITY);
    peer.syncBody(FILE, "初始甲改\n");
    peer.markDiskWrite(FILE, "初始甲改\n");
    const rebuildsBefore = c.nodes[0].stats.rebuilds;
    const baselinesBefore = c.nodes[0].stats.baselinesSent;
    c.dropAll();
    // 会话关闭（引用归零）后按磁盘最新正文重开：保留文档真值，不回退、不重建
    peer.release(FILE);
    peer.open(FILE, "初始甲改\n", IDENTITY);
    expect(c.text(0)).toBe("初始甲改\n");
    expect(c.nodes[0].stats.rebuilds).toBe(rebuildsBefore);
    expect(c.nodes[0].stats.baselinesSent).toBe(baselinesBefore);
  });

  it("重开时磁盘有未落盘差异（房间内新增）：并入内存真值，不整体回退", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "第一段\n第二段\n", IDENTITY);
    // 对端已广播、本端尚未落盘：文档与磁盘基线出现差异
    peer.syncBody(FILE, "第一段甲改\n第二段\n");
    peer.release(FILE);
    peer.open(FILE, "第一段\n第二段\n", IDENTITY);
    expect(c.text(0)).toBe("第一段甲改\n第二段\n");
  });

  it("重开时磁盘确有新增（外部改盘）：作为编辑并入，不整体回退", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "第一段\n第二段\n", IDENTITY);
    peer.syncBody(FILE, "第一段甲改\n第二段\n");
    peer.markDiskWrite(FILE, "第一段甲改\n第二段\n");
    peer.release(FILE);
    peer.open(FILE, "第一段甲改\n第二段\n外部新增\n", IDENTITY);
    expect(c.text(0)).toBe("第一段甲改\n第二段\n外部新增\n");
  });

  it("文件改名/删除销毁文档：同路径新文件不串内容", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "旧文件内容\n", IDENTITY);
    peer.destroyDoc(FILE);
    expect(peer.getText(FILE)).toBeNull();
    peer.open(FILE, "新文件内容\n", IDENTITY);
    expect(c.text(0)).toBe("新文件内容\n");
  });
});

describe("缺帧与反熵", () => {
  it("丢弃增量后由 resyncActive 握手补齐", () => {
    const c = makeCluster(2);
    c.nodes[0].peer.open(FILE, "开头\n", IDENTITY);
    c.nodes[1].peer.open(FILE, "开头\n", IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "开头甲改\n");
    c.dropAll();
    expect(c.text(1)).toBe("开头\n");
    c.nodes[0].peer.resyncActive();
    c.nodes[1].peer.resyncActive();
    c.deliver();
    expect(c.text(1)).toBe("开头甲改\n");
  });

  it("收到重同步请求帧即重发握手（对端据此补齐）", () => {
    const c = makeCluster(2);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    a.open(FILE, "开头\n", IDENTITY);
    b.open(FILE, "开头\n", IDENTITY);
    c.deliver();
    b.syncBody(FILE, "开头乙改\n");
    c.dropAll();
    a.receive(FILE, encodeNoteResync(0), 1, undefined);
    c.deliver();
    expect(c.text(0)).toBe("开头乙改\n");
  });
});

describe("边界", () => {
  it("空正文（仅 frontmatter）基线可用：随后写入正文不重复", () => {
    const c = makeCluster(2);
    c.nodes[0].peer.open(FILE, "", IDENTITY);
    c.nodes[1].peer.open(FILE, "", IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "# 标题\n\n正文\n");
    c.deliver();
    expect(c.text(1)).toBe("# 标题\n\n正文\n");
    expect(countOf(c.text(1), "# 标题")).toBe(1);
  });

  it("CRLF 正文按 LF 语义收敛，不产生重复行", () => {
    const c = makeCluster(2);
    const base = "# 标题\r\n\r\n正文甲\r\n";
    c.nodes[0].peer.open(FILE, base, IDENTITY);
    c.nodes[1].peer.open(FILE, base, IDENTITY);
    c.deliver();
    c.nodes[1].peer.syncBody(FILE, "# 标题\r\n\r\n正文乙\r\n");
    c.deliver();
    expect(countOf(c.text(0), "正文乙")).toBe(1);
    expect(c.text(0)).toBe(c.text(1));
  });

  it("远端 update 应用期间的同步窗口可识别为远端合入", () => {
    const c = makeCluster(2);
    const base = "开头\n";
    c.nodes[0].peer.open(FILE, base, IDENTITY);
    const docB = c.nodes[1].peer.open(FILE, base, IDENTITY);
    c.deliver();
    c.nodes[0].peer.syncBody(FILE, "开头甲改\n");
    const payload = c.nodes[1].inbox[0]?.payload;
    expect(payload).toBeDefined();
    const duringApply: boolean[] = [];
    docB.ytext.observe(() => duringApply.push(c.nodes[1].peer.isRemoteApplying()));
    const remoteAuthor: NoteRemoteAuthor = { id: "peer-0", name: "甲", device: "机器甲" };
    c.nodes[1].peer.receive(FILE, payload!, 0, remoteAuthor);
    expect(duringApply).toContain(true);
    expect(c.nodes[1].peer.isRemoteApplying()).toBe(false);
    expect(c.nodes[1].peer.getLastRemoteAuthor(FILE)?.name).toBe("甲");
    expect(docB.ytext.toString()).toBe("开头甲改\n");
  });
});

describe("标签平局与伪造通告（全序必须给出胜负）", () => {
  it("同 (seq, author) 不同正文：要么采纳对方，要么回通告更高的本端标签（不再互不采纳）", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "本端正文\n", IDENTITY);
    const localTag = c.tag(0)!;
    c.dropAll();

    // 伪造通告：与本端同 seq/author，但正文与内容标识不同（同一 author 的序号记忆被重置后可真实出现）
    const otherText = "对端正文\n";
    const forged: BaselineTag = {
      seq: localTag.seq,
      author: localTag.author,
      id: baselineIdOf(otherText),
    };
    expect(compareTag(forged, localTag)).not.toBe(0);
    peer.receive(FILE, encodeNoteBaseline(forged, otherText, otherText), 1);
    expect(c.deliver()).toBeLessThan(DELIVER_LIMIT);

    const after = c.tag(0)!;
    if (compareTag(forged, localTag) > 0) {
      expect(compareTag(after, forged)).toBe(0); // 伪造标签更高 → 采纳
    } else {
      expect(compareTag(after, localTag)).toBe(0); // 本端更高 → 回通告本端标签让对方采纳
      expect(c.nodes[0].stats.baselinesSent).toBeGreaterThan(0);
    }
  });

  it("两端同 author 同序号但正文不同：房间最终收敛到同一标签", () => {
    // 同一进程换会话（author 稳定、序号记忆被重置）后重发同序号、正文不同——死锁修复的端到端验证
    const c = makeCluster(2, ["同源", "同源"]);
    c.nodes[0].peer.open(FILE, "甲版本\n", IDENTITY);
    c.nodes[1].peer.open(FILE, "乙版本\n", IDENTITY);
    c.deliver();
    expect(c.text(0)).toBe(c.text(1));
    expect(compareTag(c.tag(0)!, c.tag(1)!)).toBe(0);
  });

  it("自称本端标识但基线正文不符的通告：一致性校验拒绝，且不影响本端正文与后续合法同步", () => {
    const c = makeCluster(2);
    const a = c.nodes[0].peer;
    const b = c.nodes[1].peer;
    a.open(FILE, "一\n二\n", IDENTITY);
    b.open(FILE, "一\n二\n", IDENTITY);
    c.deliver();
    const aTag = c.tag(0)!;
    c.dropAll();

    // 伪造帧：声明本端标识，但正文与其标识自相矛盾（内容标识按正文重算必然不符）
    const forged: BaselineTag = { seq: aTag.seq, author: aTag.author, id: aTag.id };
    a.receive(FILE, encodeNoteBaseline(forged, "异正文\n", "异正文\n"), 1);
    expect(c.text(0)).toBe("一\n二\n");

    // 合法对端的同步帧照常应用（该伪造帧不得污染兼容性判定）
    b.syncBody(FILE, "一\n二乙改\n");
    const fromB = c.nodes[0].inbox.at(-1)!.payload;
    c.dropAll();
    a.receive(FILE, fromB, 1, undefined);
    expect(c.text(0)).toBe("一\n二乙改\n");
    // 一轮交换即静默：伪造帧不得引发通告风暴
    expect(c.deliver()).toBeLessThan(DELIVER_LIMIT);
    expect(c.deliver()).toBe(0);
  });
});

describe("文档生命周期与保留上限", () => {
  it("保留上限：超出上限后最久未触达且无编辑面的文档被摘除，重开得到新实例", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    const first = peer.open("notes/f0.md", "内容0\n", IDENTITY);
    peer.release("notes/f0.md");
    // 每个文件开一次即释放：无编辑面的保留文档累积到上限后按最久未触达淘汰
    for (let i = 1; i < 40; i++) {
      peer.open(`notes/f${i}.md`, `内容${i}\n`, IDENTITY);
      peer.release(`notes/f${i}.md`);
    }
    expect(peer.getText("notes/f0.md")).toBeNull();
    const reopened = peer.open("notes/f0.md", "内容0\n", IDENTITY);
    expect(reopened).not.toBe(first);
    expect(reopened.ytext.toString()).toBe("内容0\n");
  });

  it("销毁后再打开：同路径得到新文档（旧实例不再被复用），且旧实例清理后不再发出 awareness 帧", async () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    const first = peer.open(FILE, "旧内容\n", IDENTITY);
    await new Promise((resolve) => setTimeout(resolve, 150)); // 等首帧身份 awareness 落定
    const awarenessBefore = c.nodes[0].stats.awarenessSent;
    expect(awarenessBefore).toBeGreaterThan(0);

    peer.destroyDoc(FILE);
    expect(peer.getText(FILE)).toBeNull();
    const second = peer.open(FILE, "新内容\n", IDENTITY);
    expect(second).not.toBe(first);
    expect(second.ytext.toString()).toBe("新内容\n");

    // 新文档首帧身份 awareness 之外不得再有帧：旧实例延后一拍销毁并关闸（destroy 会同步 emit update）
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(c.nodes[0].stats.awarenessSent).toBe(awarenessBefore + 1);
  });

  it("摘除后的旧文档立即静音：仍可写但不再发出任何帧", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    const doc = peer.open(FILE, "内容\n", IDENTITY);
    doc.ytext.insert(0, "编辑前\n");
    const sentBefore = c.nodes[0].stats.syncsSent;
    expect(sentBefore).toBeGreaterThan(0);

    peer.destroyDoc(FILE);
    // 窗口内旧文档仍被编辑面引用（销毁延后一拍）：此处的写入不得再产生出站帧
    doc.ytext.insert(0, "编辑后\n");
    expect(c.nodes[0].stats.syncsSent).toBe(sentBefore);
  });

  it("目录前缀销毁：该目录下文档一并摘除（同前缀新文件不复用旧内容）", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open("dir/a.md", "目录内旧内容\n", IDENTITY);
    peer.open("dir/sub/b.md", "更深一层\n", IDENTITY);
    peer.open("other.md", "目录外\n", IDENTITY);
    peer.disposeDocsUnder("dir");
    expect(peer.getText("dir/a.md")).toBeNull();
    expect(peer.getText("dir/sub/b.md")).toBeNull();
    expect(peer.getText("other.md")).toBe("目录外\n");
    peer.open("dir/a.md", "同路径新文件\n", IDENTITY);
    expect(peer.getText("dir/a.md")).toBe("同路径新文件\n");
  });

  it("反熵握手只覆盖有编辑面的文档（引用归零的保留文档不参与）", () => {
    const c = makeCluster(1);
    const peer = c.nodes[0].peer;
    peer.open(FILE, "内容\n", IDENTITY);
    peer.release(FILE);
    const before = c.nodes[0].stats.syncsSent;
    peer.resyncActive();
    expect(c.nodes[0].stats.syncsSent).toBe(before);

    peer.open(FILE, "内容\n", IDENTITY);
    peer.resyncActive();
    expect(c.nodes[0].stats.syncsSent).toBe(before + 1);
  });

  it("awareness 节流窗口内的多次变更合并为一帧且不丢变更（两个不同 client）", async () => {
    const c = makeCluster(1);
    const doc = c.nodes[0].peer.open(FILE, "内容\n", IDENTITY);
    await new Promise((resolve) => setTimeout(resolve, 150)); // 身份首帧落定

    // 注入一个远端 client 的 awareness 状态，制造「同窗口两个不同 client」：覆盖式实现只会发出后者
    const remoteDoc = new Y.Doc();
    const remoteAwareness = new Awareness(remoteDoc);
    remoteAwareness.setLocalStateField("user", { name: "对端", color: "#ff0000" });
    applyAwarenessUpdate(
      doc.awareness,
      encodeAwarenessUpdate(remoteAwareness, [remoteAwareness.clientID]),
      null,
    );
    const before = c.nodes[0].stats.awarenessSent;
    doc.awareness.setLocalStateField("cursor", { anchor: 1 });

    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(c.nodes[0].stats.awarenessSent).toBe(before + 1);
    const ids = awarenessClients(c.nodes[0].awarenessFrames.at(-1)!);
    expect(ids).toContain(remoteAwareness.clientID);
    expect(ids).toContain(doc.awareness.clientID);
    remoteAwareness.destroy();
    remoteDoc.destroy();
  });
});
