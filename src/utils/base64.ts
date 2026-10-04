/**
 * 二进制 <-> Base64 互转（浏览器环境，二进制安全）。
 *
 * 协作频道为 JSON 文本帧，Yjs 的二进制同步/awareness 编码无法直接传输，
 * 统一经 base64 包装进 `note-sync`/`note-aware` 消息（传输层不透明透传）。
 * chunked 循环防大文本时栈溢出；传入/传出均为 Uint8Array。
 */
const CHUNK = 0x8000;

/** Uint8Array → base64 字符串（二进制安全，逐块 btoa 防栈溢出）。 */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

/** base64 字符串 → Uint8Array（二进制安全；容错：源字符串可能含非 base64 噪声则抛错）。 */
export function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** dataURL → 文本（非 dataURL 原样返回）。严格按 UTF-8 解码：解不出来即抛错，由调用方标「无法解析」。
 *
 * 为什么不用宽松解码：二进制附件（PDF/zip）宽松解码会得到一段乱码且不报错，调用方据此认为「解析成功」，
 * 于是乱码被当正文注入模型、节点也不再显示「无法解析」提示——宁可如实标失败。
 */
export function dataUrlToText(dataUrl: string): string {
  const comma = dataUrl.indexOf(",");
  if (!dataUrl.startsWith("data:") || comma < 0) return dataUrl;
  const binary = atob(dataUrl.slice(comma + 1));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}
