/**
 * 图片扩展名与 MIME 的单一真源。
 *
 * 「哪些扩展名算图片」这份知识原先散在多个调用点（画布媒体判定、白板节点分类、表格图片落盘、
 * 协作空间附件读写、文件选择器 accept），任一处遗漏就会互相分叉。本模块统一承载，其余位置只取用：
 * 集合用 IMAGE_EXTS，文件名后缀判定用 isImageFileName，MIME 用 imageMimeFromExt。
 *
 * 注意：本地附件 dataURL 的 mime 由 Rust 侧 `commands/vault.rs` 的同一张表产出，属另一语言实现；
 * 改动本表时须同步该处。
 */

/** 图片扩展名 → MIME。本表即扩展名集合的真源，本模块其余导出全部由它派生。 */
const IMAGE_MIME = new Map<string, string>([
  ["png", "image/png"],
  ["jpg", "image/jpeg"],
  ["jpeg", "image/jpeg"],
  ["webp", "image/webp"],
  ["gif", "image/gif"],
]);

/** 图片扩展名集合（小写、无点号）。 */
export const IMAGE_EXTS: readonly string[] = [...IMAGE_MIME.keys()];

/**
 * 文件名是否以图片扩展名结尾（大小写不敏感）。
 * 按扩展名而非 `File.type` 判定：未知扩展名与部分粘贴来源下 MIME 为空，按 MIME 判会与文件树拖入路径分叉。
 */
export function isImageFileName(name: string): boolean {
  const m = /\.([A-Za-z0-9]+)$/.exec(name);
  return m !== null && IMAGE_MIME.has(m[1].toLowerCase());
}

/** 按扩展名取 MIME（非图片返回 null；回落值由调用方按各自语义决定）。 */
export function imageMimeFromExt(ext: string): string | null {
  return IMAGE_MIME.get(ext.toLowerCase()) ?? null;
}

/** `<input accept>` 用的图片 MIME 列表（同一 MIME 只列一次）。 */
export const IMAGE_ACCEPT = [...new Set(IMAGE_MIME.values())].join(",");
