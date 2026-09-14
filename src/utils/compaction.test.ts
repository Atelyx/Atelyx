/**
 * 会话压缩注解契约测试（utils/compaction）。
 *
 * 核心不变式：注解**非破坏**——锚点缺失（被回滚/分支丢弃）一律退回完整历史，绝不静默丢消息；
 * 压缩覆盖全部消息（含最新一轮），故标记行下标可等于列表长度（渲染在末尾）。
 */
import { describe, expect, it } from "vitest";
import {
  compactionMarkerIndex,
  nextCompactionBoundary,
  splitByCompaction,
} from "./compaction";
import type { ConversationCompaction } from "@/types";

describe("splitByCompaction", () => {
  const msgs = [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }];
  const compaction: ConversationCompaction = {
    summary: "摘要",
    upToMessageId: "b",
    messageCount: 2,
    createdAt: 0,
  };

  it("锚点及其之前剔除，之后保留", () => {
    expect(splitByCompaction(msgs, compaction)).toEqual({
      kept: [{ id: "c" }, { id: "d" }],
      checkpoint: compaction,
    });
  });

  it("无注解 = 全量历史", () => {
    expect(splitByCompaction(msgs)).toEqual({ kept: msgs, checkpoint: null });
  });

  it("锚点缺失（被回滚/分支丢弃）视为失效，退回完整历史不静默丢内容", () => {
    expect(splitByCompaction(msgs, { ...compaction, upToMessageId: "gone" })).toEqual({
      kept: msgs,
      checkpoint: null,
    });
  });

  it("锚点 = 最后一条 = 全部历史被摘要替代（模型侧只剩摘要）", () => {
    expect(splitByCompaction(msgs, { ...compaction, upToMessageId: "d" })).toEqual({
      kept: [],
      checkpoint: { ...compaction, upToMessageId: "d" },
    });
  });
});

describe("compactionMarkerIndex", () => {
  const msgs = [{ id: "a" }, { id: "b" }, { id: "c" }];
  it("锚点之后的第一条（与历史重建同判定）", () => {
    expect(
      compactionMarkerIndex(msgs, { summary: "s", upToMessageId: "b", messageCount: 2, createdAt: 0 }),
    ).toBe(2);
  });
  it("锚点 = 最后一条时返回列表长度（标记渲染在末尾）", () => {
    expect(
      compactionMarkerIndex(msgs, { summary: "s", upToMessageId: "c", messageCount: 3, createdAt: 0 }),
    ).toBe(msgs.length);
  });
  it("无注解/锚点缺失返回 -1（不渲染标记）", () => {
    expect(compactionMarkerIndex(msgs)).toBe(-1);
    expect(
      compactionMarkerIndex(msgs, { summary: "s", upToMessageId: "gone", messageCount: 1, createdAt: 0 }),
    ).toBe(-1);
  });
});

describe("nextCompactionBoundary", () => {
  const conv = (n: number) =>
    Array.from({ length: n }, (_, i) => ({ id: `m${i}`, role: i % 2 ? "assistant" : "user" }));

  it("全部消息（含最新一轮）折进检查点，锚点 = 最后一条", () => {
    expect(nextCompactionBoundary(conv(4))).toEqual({
      upToMessageId: "m3",
      messageCount: 4,
    });
  });

  it("不足一轮问答（<2 条）返回 null", () => {
    expect(nextCompactionBoundary(conv(0))).toBeNull();
    expect(nextCompactionBoundary(conv(1))).toBeNull();
  });

  it("已覆盖到最后一条（无新增）返回 null", () => {
    const list = conv(4);
    expect(
      nextCompactionBoundary(list, {
        summary: "s",
        upToMessageId: "m3",
        messageCount: 4,
        createdAt: 0,
      }),
    ).toBeNull();
  });

  it("压缩后追加新消息即可再次压缩（边界前进到新末条）", () => {
    const list = conv(5);
    expect(
      nextCompactionBoundary(list, {
        summary: "s",
        upToMessageId: "m3",
        messageCount: 4,
        createdAt: 0,
      }),
    ).toEqual({ upToMessageId: "m4", messageCount: 5 });
  });

  it("锚点失效（旧注解被回滚）允许重算", () => {
    expect(
      nextCompactionBoundary(conv(4), {
        summary: "s",
        upToMessageId: "gone",
        messageCount: 2,
        createdAt: 0,
      }),
    ).toEqual({ upToMessageId: "m3", messageCount: 4 });
  });
});
