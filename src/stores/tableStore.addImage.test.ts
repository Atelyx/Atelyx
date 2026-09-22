/**
 * 表格图片相关：单元格导入的大小预检 + xlsx 导出前把图片路径经内容面读回为 dataURL。
 * 预检理由：超过单文件字节上限的图片必须在读取/上传前拒绝——读成 base64 再被服务端按
 * 解码后字节拒绝是纯浪费，且服务端状态码对用户不可读。
 * 导出内联理由：导出命令只认 dataURL 或本地仓库附件路径；协作空间附件不在本机磁盘，
 * 须先解析为 dataURL 再交给导出（本机与空间同一路径）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { TABLE_IMAGE_MAX_BYTES } from "@/constants/table";

const h = vi.hoisted(() => ({
  importTableImage: vi.fn(),
  exportTableXlsx: vi.fn(),
  readAttachmentDataUrl: vi.fn(),
  saveFile: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// 插件注册表会把全量领域 store 拉进模块图并在 ESM 初始化期互相取用未就绪的 store，桩掉斩断这条环。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

vi.mock("@/services/table", () => ({
  importTableImage: h.importTableImage,
  readTableVault: vi.fn(),
  writeTableVault: vi.fn(),
  patchTableVault: vi.fn(async () => null),
  cleanupTableAttachments: vi.fn(async () => 0),
  exportTableXlsx: h.exportTableXlsx,
  saveImageToDownloads: vi.fn(),
}));

vi.mock("@/services/vault", () => ({
  readAttachmentDataUrl: h.readAttachmentDataUrl,
}));

vi.mock("@/services/dialog", () => ({
  saveFile: h.saveFile,
}));

type TableStore = typeof import("./tableStore");

let table: TableStore;

beforeEach(async () => {
  vi.resetModules();
  h.importTableImage.mockReset();
  h.exportTableXlsx.mockReset();
  h.readAttachmentDataUrl.mockReset();
  h.saveFile.mockReset();
  // 先起 collabStore 再取 tableStore：tableStore 的协作接线在模块加载期读 collabStore 服务面
  await import("./collabStore");
  table = await import("./tableStore");
  table.useTableStore.setState({
    tableFile: "t.atb",
    id: "t1",
    title: "表",
    fields: [{ id: "f1", name: "图", type: "image" }] as never,
    rows: [{ id: "r1", values: {} }] as never,
  });
});

describe("addImageToCell 大小预检", () => {
  it("超过单文件字节上限：不发起导入（零网络传输），错误提示含上限", async () => {
    const file = new File([new Uint8Array([1])], "big.png");
    // File 的 size 由内容派生；用实例属性覆盖免掉真分配 50MB 缓冲
    Object.defineProperty(file, "size", { value: TABLE_IMAGE_MAX_BYTES + 1 });

    await table.useTableStore.getState().addImageToCell("r1", "f1", file);

    expect(h.importTableImage).not.toHaveBeenCalled();
    expect(table.useTableStore.getState().error).toContain("50MB");
  });

  it("上限内：正常发起导入并写入单元格", async () => {
    h.importTableImage.mockResolvedValue(".space-media/tables/t1/img-x.png");
    const file = new File([new Uint8Array([1, 2, 3])], "ok.png");
    Object.defineProperty(file, "size", { value: TABLE_IMAGE_MAX_BYTES });

    await table.useTableStore.getState().addImageToCell("r1", "f1", file);

    expect(h.importTableImage).toHaveBeenCalledTimes(1);
    expect(table.useTableStore.getState().error).toBeNull();
    expect(table.useTableStore.getState().rows[0].values.f1).toEqual({
      images: [".space-media/tables/t1/img-x.png"],
    });
    // 取消导入落盘触发的防抖保存 timer，不跨测试残留
    table.useTableStore.getState().clear();
  });
});

describe("exportXlsx 图片内联", () => {
  it("路径引用经内容面读回 dataURL 后交给导出，且不改动内存态（快照写时克隆）", async () => {
    h.saveFile.mockResolvedValue("C:/out.xlsx");
    h.readAttachmentDataUrl.mockResolvedValue("data:image/png;base64,AAAA");
    table.useTableStore.setState({
      rows: [{ id: "r1", values: { f1: { images: ["attachments/a.png"] } } }] as never,
    });

    expect(await table.useTableStore.getState().exportXlsx()).toBe(true);

    expect(h.readAttachmentDataUrl).toHaveBeenCalledWith("attachments/a.png");
    const snapshot = h.exportTableXlsx.mock.calls[0][0] as { rows: { values: Record<string, unknown> }[] };
    expect(snapshot.rows[0].values.f1).toEqual({ images: ["data:image/png;base64,AAAA"] });
    // 内存态仍是路径引用：就地改会把 dataURL 落盘并随协作补丁广播
    expect(table.useTableStore.getState().rows[0].values.f1).toEqual({
      images: ["attachments/a.png"],
    });
    table.useTableStore.getState().clear();
  });

  it("已是 dataURL 的条目不再读回（零多余 I/O）", async () => {
    h.saveFile.mockResolvedValue("C:/out.xlsx");
    table.useTableStore.setState({
      rows: [{ id: "r1", values: { f1: { images: ["data:image/png;base64,BBBB"] } } }] as never,
    });

    expect(await table.useTableStore.getState().exportXlsx()).toBe(true);
    expect(h.readAttachmentDataUrl).not.toHaveBeenCalled();
    table.useTableStore.getState().clear();
  });
});
