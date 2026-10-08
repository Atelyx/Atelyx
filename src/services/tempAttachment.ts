/**
 * 未入库附件（临时区）service（对应 Rust `commands/temp_attachment.rs`）：字节落仓库内隐藏目录，
 * 实体只存**仓库相对路径**引用；引用形态判定（`isTempAttachmentRef`）在 `utils/tempAttachmentPath`。
 */
import type { TempComponent } from "@/utils/tempAttachmentPath";
import { bytesToBase64, dataUrlToText } from "@/utils/base64";
import { readAttachmentDataUrl } from "@/services/vault";
import { getActiveContentBackend } from "@/services/content/factory";

/**
 * 附件字节写入临时区（`.atelyx/temp/<组件>/<实例目录>/`），返回仓库相对路径引用。
 * 不随 `.atlx` 内嵌 base64——图片会让画布文件涨到几十 MB。
 * `component` = 附件归属组件（画布 / AI 会话；表格图片走 `importTableImage`）；
 * `instanceId` = 实体稳定 id（画布 `.atlx` 内 id / 会话 id），后端派生成无路径语义的实例目录名。
 */
export async function writeTempAttachment(
  component: TempComponent,
  instanceId: string,
  fileName: string,
  source: File,
): Promise<string> {
  const bytes = new Uint8Array(await source.arrayBuffer());
  return getActiveContentBackend().writeTempAttachment(component, instanceId, fileName, bytesToBase64(bytes));
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
  const result = await getActiveContentBackend().importAttachment(ref, fileName);
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
  return getActiveContentBackend().cleanupCanvasTempAttachments(canvasId, canvasFile);
}

/**
 * 按引用回收某会话的未入库附件（AI 对话面板会话删除时调用）。
 * 返回删除文件数；失败上抛由调用方降级（回收是清理动作，不阻塞会话删除本身）。
 */
export async function cleanupSessionTempAttachments(
  sessionId: string,
  sessionFile: string,
): Promise<number> {
  return getActiveContentBackend().cleanupSessionTempAttachments(sessionId, sessionFile);
}

/**
 * 消息附件补齐读取器工厂（发送前按引用读回附件内容，画布对话节点与 AI 对话面板共用）：
 * - 二进制附件（不是 UTF-8 文本）记入 `nonTextRefs` 并返回空串——预期不注入模型，不算失败；
 * - 读不到（已删/权限）回调 `onReadFailure` 后返回空串——该附件不进本轮请求，
 *   其余附件与对话照常（按附件粒度降级，抛错会中断整轮）。
 * `nonTextRefs` 由调用方持有并负责生命周期（切仓库/载入时清空），水合等流程可与读取器共用同一集合。
 */
export function createMessageAttachmentReader(
  onReadFailure: (ref: string, error: unknown) => void,
  nonTextRefs: Set<string> = new Set(),
): (ref: string, kind: "image" | "file") => Promise<string> {
  return async (ref, kind) => {
    if (nonTextRefs.has(ref)) return "";
    try {
      if (kind === "file") {
        const text = await readAttachmentText(ref);
        if (text === null) {
          nonTextRefs.add(ref);
          return "";
        }
        return text;
      }
      return await readAttachmentRef(ref, kind);
    } catch (e) {
      onReadFailure(ref, e);
      return "";
    }
  };
}
