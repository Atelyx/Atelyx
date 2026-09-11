/**
 * 笔记协作帧编解码（`note-sync` 通道载荷）。
 *
 * 帧首 varUint = 帧类型，后续字段按类型排布；`SYNC` 帧内嵌 y-protocols 同步消息
 * （syncStep1/syncStep2/update 原样透传）。
 *
 * 帧必须携带发送方**基线标签**：不同基线文本的文档合并会按本地状态向量截断对端 struct 内容，
 * 产生「尾部混入异文本」或静默分歧，因此接收方按标签决定是否应用（见 notePeer）。
 * 标签含 `id` = 基线文本的内容标识：两端各自建立**同一正文**基线时（同文本不同 `seq`/`author`）
 * 直接判定兼容、按普通 CRDT 增量合并，无需重建文档。
 *
 * 解码对任意字节串都返回结果或 null，不抛异常（网络载荷不可信）。
 */
import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";

/** 同步帧类型（内嵌 y-protocols 消息）。 */
export const NOTE_FRAME_SYNC = 0x03;
/** 基线通告帧类型（携带基线全文）。 */
export const NOTE_FRAME_BASELINE = 0x42;
/** 重同步请求帧类型（relay 缺帧/周期反熵）。 */
export const NOTE_FRAME_RESYNC = 0x43;

/**
 * 基线标签：`seq` 为提案序号（Lamport），`author` 为本会话稳定身份 id，`id` 为基线正文内容标识。
 * 全序为 `seq → author → id`（三段，绝无平局）；兼容性只看 `id`（同正文的基线字节一致，合并幂等）。
 */
export interface BaselineTag {
  seq: number;
  author: string;
  id: string;
}

/**
 * 基线正文内容标识 = 长度 + 两个进位不同的 32 位散列（合计 64 位，碰撞概率可忽略）。
 * 仅作快速判等；通告帧另附带全文，接收方可逐字符复核（见 notePeer 的实证不兼容集合）。
 */
export function baselineIdOf(text: string): string {
  let h1 = 0x811c9dc5;
  let h2 = 0x9e3779b9;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ code, 0x01000193);
    h2 = Math.imul(h2 ^ code, 0x85ebca6b);
  }
  return `${text.length.toString(36)}-${(h1 >>> 0).toString(36)}-${(h2 >>> 0).toString(36)}`;
}

/**
 * 基线标签全序：`seq` → `author` → `id`（三段全序，绝无平局）。
 * 平局必须有确定胜负：两端标签同 `seq`/`author` 但正文不同时（同一进程的序号记忆被重置后重发同序号），
 * 无胜负会让双方都只回通告、永不采纳，导致该文件永久不合并。
 */
export function compareTag(a: BaselineTag, b: BaselineTag): number {
  if (a.seq !== b.seq) return a.seq < b.seq ? -1 : 1;
  if (a.author !== b.author) return a.author < b.author ? -1 : 1;
  if (a.id === b.id) return 0;
  return a.id < b.id ? -1 : 1;
}

/** 基线正文相同（字节一致，合并幂等）：只比内容标识。 */
export function sameContent(a: BaselineTag, b: BaselineTag): boolean {
  return a.id === b.id;
}

function writeTag(encoder: encoding.Encoder, tag: BaselineTag): void {
  encoding.writeVarUint(encoder, tag.seq);
  encoding.writeVarString(encoder, tag.author);
  encoding.writeVarString(encoder, tag.id);
}

/** 同步帧：基线标签 + y-protocols 同步消息。 */
export function encodeNoteSync(tag: BaselineTag, payload: Uint8Array): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, NOTE_FRAME_SYNC);
  writeTag(encoder, tag);
  encoding.writeVarUint8Array(encoder, payload);
  return encoding.toUint8Array(encoder);
}

/** 基线通告帧：基线标签 + 基线正文（据以重建同一 seed）+ 发送方当前正文（据以三方合并）。 */
export function encodeNoteBaseline(
  tag: BaselineTag,
  baselineText: string,
  currentText: string,
): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, NOTE_FRAME_BASELINE);
  writeTag(encoder, tag);
  encoding.writeVarString(encoder, baselineText);
  encoding.writeVarString(encoder, currentText);
  return encoding.toUint8Array(encoder);
}

/** 重同步请求帧：reason 取调用方语义（0 = relay 缺帧，1 = 周期反熵）。 */
export function encodeNoteResync(reason: number): Uint8Array {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, NOTE_FRAME_RESYNC);
  encoding.writeVarUint(encoder, reason);
  return encoding.toUint8Array(encoder);
}

/** 解码结果（按帧类型区分）。 */
export type NoteFrame =
  | { kind: "sync"; tag: BaselineTag; payload: Uint8Array }
  | { kind: "baseline"; tag: BaselineTag; baselineText: string; currentText: string }
  | { kind: "resync"; reason: number };

/** 解析帧：类型/字段不完整或类型未知一律返回 null。 */
export function decodeNoteFrame(payload: Uint8Array): NoteFrame | null {
  try {
    const decoder = decoding.createDecoder(payload);
    if (!decoding.hasContent(decoder)) return null;
    const kind = decoding.readVarUint(decoder);
    if (kind === NOTE_FRAME_SYNC) {
      const tag = readTag(decoder);
      const inner = decoding.readVarUint8Array(decoder);
      if (decoding.hasContent(decoder)) return null;
      return { kind: "sync", tag, payload: inner };
    }
    if (kind === NOTE_FRAME_BASELINE) {
      const tag = readTag(decoder);
      const baselineText = decoding.readVarString(decoder);
      const currentText = decoding.readVarString(decoder);
      if (decoding.hasContent(decoder)) return null;
      return { kind: "baseline", tag, baselineText, currentText };
    }
    if (kind === NOTE_FRAME_RESYNC) {
      const reason = decoding.readVarUint(decoder);
      if (decoding.hasContent(decoder)) return null;
      return { kind: "resync", reason };
    }
    return null;
  } catch {
    return null;
  }
}

function readTag(decoder: decoding.Decoder): BaselineTag {
  const seq = decoding.readVarUint(decoder);
  const author = decoding.readVarString(decoder);
  const id = decoding.readVarString(decoder);
  return { seq, author, id };
}
