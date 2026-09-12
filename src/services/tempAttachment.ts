/**
 * 未入库附件（临时区）service —— 对应 Rust `commands/temp_attachment.rs`。
 *
 * 画布对话节点的附件**不随 `.atlx` 内嵌 base64**（否则图片会让画布文件涨到几十 MB）：
 * 粘贴/拖入时把字节写进仓库内隐藏目录 `.atelyx/temp/<canvasKey>/`，画布只存**仓库相对路径**引用。
 * 路径放仓库内（而非应用数据目录）换来三件事不必另造一套边界：
 * - 读回复用仓库附件读命令（`read_attachment_data_url`，`safe_join` 自带越界校验）；
 * - 回收天然按仓库归属（不会跨仓库误删其它仓库画布的附件）；
 * - 与表格图片（`.atelyx/attachments/<tableId>/`）同族，文件树与 watcher 天然跳过。
 *
 * 引用形态判定（`isTempAttachmentRef`）在 `utils/tempAttachmentPath`（纯函数，无 I/O）。
 */
import { invoke } from "@tauri-apps/api/core";
import { TEMP_ATTACHMENT_DIR } from "@/utils/tempAttachmentPath";
import { bytesToBase64 } from "@/utils/base64";
import { readAttachmentDataUrl } from "@/services/vault";

/**
 * 附件字节写入临时区，返回仓库相对路径引用。
 * `canvasId` = 画布稳定 id（`.atlx` 内 id），后端派生成定长十六进制目录名（不带路径语义）。
 */
export async function writeTempAttachment(
  canvasId: string,
  fileName: string,
  source: File,
): Promise<string> {
  const bytes = new Uint8Array(await source.arrayBuffer());
  return invoke<string>("write_temp_attachment", {
    canvasId,
    fileName,
    base64Data: bytesToBase64(bytes),
  });
}

/**
 * 按引用读附件内容：
 * - 图片 → dataURL（直接可渲染 / 可发给模型）；
 * - 文本类 → 解出文本（dataURL 里是 base64，这里转回字符串）；
 * - 其余二进制 → dataURL（调用方自行消费）。
 *
 * 为什么统一走 dataURL 通道：仓库附件读命令只认「仓库内路径」，临时区在仓库内、同一条命令即可，
 * 不为临时区另开读取端口。文本类附件在这里把 base64 载荷转回字符串。
 */
export async function readAttachmentRef(
  ref: string,
  kind: "image" | "file",
): Promise<string> {
  const dataUrl = await readAttachmentDataUrl(ref);
  if (kind === "image") return dataUrl;
  return dataUrlToText(dataUrl);
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

/**
 * 读文本类附件内容，区分两种失败：
 * - 文件读不到（已删/权限）→ 抛错，调用方按「文件缺失」处理；
 * - 内容不是 UTF-8 文本（二进制附件）→ 返回 null，调用方按「无法解析」处理。
 * 两种失败的用户可见语义不同（文件没了 vs 本来就不是文本），不能混成一个错误。
 */
export async function readAttachmentText(ref: string): Promise<string | null> {
  const dataUrl = await readAttachmentDataUrl(ref);
  try {
    return dataUrlToText(dataUrl);
  } catch {
    return null;
  }
}

/**
 * 把未入库附件复制进仓库附件文件夹，返回仓库相对路径（画布改用该路径引用）。
 * `fileName` = 附件显示名（调用方手上就有）：后端据此定落位名，不去反解临时叶子名里的随机前缀。
 */
export async function importVaultAttachment(ref: string, fileName: string): Promise<string> {
  const result = await invoke<{ file: string }>("import_vault_attachment", {
    rel: ref,
    fileName,
  });
  return result.file;
}

/**
 * 按引用回收某画布的未入库附件（画布关闭/删除后调用）。
 * 返回删除文件数；会话内不调用——撤销需要能恢复引用。
 */
export async function cleanupCanvasTempAttachments(
  canvasId: string,
  canvasFile: string,
): Promise<number> {
  return invoke<number>("cleanup_canvas_temp_attachments", { canvasId, canvasFile });
}

export { TEMP_ATTACHMENT_DIR };
