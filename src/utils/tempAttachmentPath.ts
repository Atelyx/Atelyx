/**
 * 附件引用形态判定（纯函数，无 I/O）。
 *
 * 仓库内附件引用有两种形态：
 * - **未入库的临时附件**：`.atelyx/temp/<canvasKey>/<文件名>`（相对仓库根，隐藏目录）。
 *   画布对话节点粘贴/拖入的附件先落在这里，画布只存路径引用——`.atlx` 不再内嵌 base64
 *   （否则图片会让画布文件涨到几十 MB）。`canvasKey` 由后端从画布 id 派生（定长十六进制），
 *   前端只按前缀判定形态、不拼目录名。
 * - **已入库的仓库附件**：`<附件文件夹>/<文件名>`（相对仓库根，任意位置）。
 *
 * 放 `utils/` 而非 service：这是被 service 层与 store/组件一同消费的契约判定，
 * 留在 service 里会让 service 之间产生循环依赖。
 */

/** 未入库临时附件的目录（相对仓库根）。与 Rust 侧 `TEMP_ATTACHMENT_DIR` 同值。 */
export const TEMP_ATTACHMENT_DIR = ".atelyx/temp";

/** 该引用是否指向未入库的临时附件。 */
export function isTempAttachmentRef(ref: string | undefined | null): boolean {
  return !!ref && ref.startsWith(`${TEMP_ATTACHMENT_DIR}/`);
}
