/**
 * 附件引用形态判定（纯函数，无 I/O）。
 *
 * 仓库内附件引用有两种形态：
 * - **未入库的临时附件 / 表格图片**：`.atelyx/temp/<组件>/<实例目录>/<文件名>`（相对根，隐藏目录）。
 *   画布对话节点粘贴/拖入的附件、AI 面板会话附件、表格图片都先落在这里，实体只存路径引用——
 *   `.atlx` 不再内嵌 base64（否则图片会让画布文件涨到几十 MB）。个人仓库与协作空间**同构**：
 *   临时区根同为 `.atelyx/temp`，实例目录名两端同算法派生（画布/会话 = 实例 id 的 FNV 哈希，
 *   表格 = 原始 tableId）——空间退化为本地仓库时媒体文件按同名相对路径落位，引用不改写。
 * - **已入库的仓库附件**：`<附件文件夹>/<文件名>`（相对根，任意位置）。
 *
 * 放 `utils/` 而非 service：这是被 service 层与 store/组件一同消费的契约判定，
 * 留在 service 里会让 service 之间产生循环依赖。
 */

/** 未入库临时附件的目录（相对根）。与 Rust 侧 `TEMP_ATTACHMENT_DIR` 同值，两端同构。 */
export const TEMP_ATTACHMENT_DIR = ".atelyx/temp";

/** 临时区组件：一类把附件落进临时区的实体。与 Rust 侧 `TempComponent` 标识同值。 */
export type TempComponent = "canvas" | "session" | "table";

/** 组件目录名（temp 下的固定一层，进引用路径、不可改）。与 Rust 侧 `dir_name` 同值。 */
export const TEMP_COMPONENT_DIRS: Record<TempComponent, string> = {
  canvas: "canvas",
  session: "sessions",
  table: "tables",
};

/**
 * 实例 id → 实例目录名：FNV-1a（64bit）十六进制串，稳定、定长、无路径语义。
 * 算法与 Rust 侧 `instance_temp_key` 逐字节一致（UTF-8 字节序），跨语言由测试向量钉死。
 *
 * 为什么不直接用画布/会话 id：它来自实体文件内容（可被外部构造/同步），直接拼进路径就得再写
 * 一套穿越校验；派生排除分隔符/`..`/保留名，路径拼装之外不再需要额外校验。碰撞由实例标记
 * 兜底：目录标记与当前实例 id 不一致即视为他人目录，不删。
 */
export function tempInstanceKey(instanceId: string): string {
  // FNV-1a 64：offset basis 0xcbf29ce484222325，prime 0x100000001b3，乘法按 2^64 截断
  let hash = 0xcbf29ce484222325n;
  const bytes = new TextEncoder().encode(instanceId);
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return hash.toString(16).padStart(16, "0");
}

/** 实例目录叶子名：画布/会话 = id 的 FNV 派生；表格 = 原始 tableId（应用生成，写入端校验）。 */
export function tempInstanceLeaf(component: TempComponent, instanceId: string): string {
  return component === "table" ? instanceId : tempInstanceKey(instanceId);
}

/** 该引用是否指向未入库的临时附件。 */
export function isTempAttachmentRef(ref: string | undefined | null): boolean {
  return !!ref && ref.startsWith(`${TEMP_ATTACHMENT_DIR}/`);
}
