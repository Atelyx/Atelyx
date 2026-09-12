/**
 * 未入库附件读取契约测试（services/tempAttachment.ts）。
 *
 * 两条语义必须分开（混用会让二进制附件出问题——历史缺陷：严格解码抛错直接中断整轮发送）：
 * - 读不到文件（已删/权限）→ 抛错，调用方按「文件缺失」处理；
 * - 内容不是 UTF-8 文本（PDF/zip 等）→ `readAttachmentText` 返回 null，按「无法解析」处理。
 */
import { describe, it, expect, vi } from "vitest";

const h = vi.hoisted(() => ({
  /** read_attachment_data_url 的返回值（dataURL）。 */
  dataUrl: "",
  /** 是否让读命令抛错（模拟文件缺失）。 */
  failRead: false,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (cmd !== "read_attachment_data_url") return null;
    if (h.failRead) throw new Error("附件不存在");
    return h.dataUrl;
  },
}));

/** 把字节编码成 dataURL（测试侧构造「是文本 / 不是文本」两类载荷）。 */
function dataUrlOf(bytes: number[], mime = "application/octet-stream"): string {
  return `data:${mime};base64,${btoa(String.fromCharCode(...bytes))}`;
}

describe("附件内容读取语义", () => {
  it("UTF-8 文本：读出原文", async () => {
    const { readAttachmentText } = await import("./tempAttachment");
    h.dataUrl = dataUrlOf([...new TextEncoder().encode("你好, world")], "text/plain");
    expect(await readAttachmentText(".atelyx/temp/k/a.txt")).toBe("你好, world");
  });

  it("非 UTF-8 字节（二进制）：返回 null 而不是抛错", async () => {
    const { readAttachmentText } = await import("./tempAttachment");
    // 0xFF 0xFE 不是合法 UTF-8 序列（PDF/zip 常见起始字节）
    h.dataUrl = dataUrlOf([0xff, 0xfe, 0x00, 0x01]);
    expect(await readAttachmentText(".atelyx/temp/k/a.pdf")).toBeNull();
  });

  it("文件读不到：抛错（调用方按文件缺失处理）", async () => {
    const { readAttachmentText } = await import("./tempAttachment");
    h.failRead = true;
    await expect(readAttachmentText(".atelyx/temp/k/gone.pdf")).rejects.toThrow();
    h.failRead = false;
  });

  it("dataUrlToText 对非 UTF-8 抛错（严格解码，不让乱码冒充正文）", async () => {
    const { dataUrlToText } = await import("./tempAttachment");
    expect(() => dataUrlToText(dataUrlOf([0xff, 0xfe]))).toThrow();
  });
});
