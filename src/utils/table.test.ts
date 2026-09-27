/**
 * 表格工具纯函数测试（utils/table）。
 *
 * 覆盖历史版本可读化（`diffTableVersions`/`summarizeTableSnapshot`）、图片值归一化与双形态
 * 比对、选中区域归约与复制粘贴
 * （`selectionRegion`/`parseTsv`/`buildRegionTsv`/`applyPasteGrid`）。
 */
import { describe, it, expect } from "vitest";
import {
  applyPasteGrid,
  buildPluginTableSnapshot,
  buildRegionTsv,
  cellToClipboardText,
  cellValueEqual,
  diffTableVersions,
  normalizeTableRow,
  parseTsv,
  selectionRegion,
  summarizeTableSnapshot,
} from "./table";
import type { CellValue, CollabPeer, TableField, TableRow } from "@/types";

const field = (id: string, name: string): TableField => ({ id, name, type: "text" });
const row = (id: string, values: Record<string, string>): TableRow => ({ id, values });

/** `.atb` 格式快照（历史存的就是这个形态）。 */
function tableSnap(fields: TableField[], rows: TableRow[]): string {
  return JSON.stringify({
    schema: "atelyx-table/v1",
    id: "t1",
    title: "分镜",
    fields,
    rows,
    createdAt: 0,
    updatedAt: 0,
  });
}

describe("diffTableVersions", () => {
  it("行新增 + 单元格修改（两版共有的字段）", () => {
    const prev = tableSnap(
      [field("f1", "镜号"), field("f2", "状态")],
      [row("r1", { f1: "01", f2: "待拍" })],
    );
    const next = tableSnap(
      [field("f1", "镜号"), field("f2", "状态")],
      [row("r1", { f1: "01", f2: "已拍" }), row("r2", { f1: "02", f2: "待拍" })],
    );
    const d = diffTableVersions(prev, next);
    expect(d.addedRows).toContain("02");
    expect(d.cellChanges).toHaveLength(1);
    expect(d.cellChanges[0]).toMatchObject({ rowIndex: 1, fieldName: "状态", from: "待拍", to: "已拍" });
  });

  it("字段删除单独计入（被删字段里的改动不重复算单元格修改）", () => {
    const prev = tableSnap(
      [field("f1", "镜号"), field("f2", "状态")],
      [row("r1", { f1: "01", f2: "待拍" })],
    );
    const next = tableSnap([field("f1", "镜号")], [row("r1", { f1: "01" })]);
    const d = diffTableVersions(prev, next);
    expect(d.removedFields.map((f) => f.name)).toEqual(["状态"]);
    expect(d.cellChanges).toHaveLength(0);
  });

  it("字段改名与列顺序变化", () => {
    const prev = tableSnap([field("f1", "镜号"), field("f2", "景别")], []);
    const next = tableSnap([field("f2", "景别"), field("f1", "分镜号")], []);
    const d = diffTableVersions(tableSnap([field("f1", "镜号")], []), tableSnap([field("f1", "分镜号")], []));
    expect(d.renamedFields).toEqual([{ from: "镜号", to: "分镜号" }]);
    const d2 = diffTableVersions(prev, next);
    expect(d2.fieldOrderChanged).toBe(true);
  });

  it("首版（prev 空）全部计为新增", () => {
    const d = diffTableVersions("", tableSnap([field("f1", "镜号")], [row("r1", { f1: "01" })]));
    expect(d.addedFields).toHaveLength(1);
    expect(d.addedRows).toHaveLength(1);
    expect(summarizeTableSnapshot("", tableSnap([field("f1", "镜号")], [row("r1", { f1: "01" })]))).toBe(
      "新建 · 1 字段 · 1 行",
    );
  });
});

describe("summarizeTableSnapshot", () => {
  it("新增行 + 修改单元格", () => {
    const prev = tableSnap([field("f1", "镜号"), field("f2", "状态")], [row("r1", { f1: "01", f2: "待拍" })]);
    const next = tableSnap(
      [field("f1", "镜号"), field("f2", "状态")],
      [row("r1", { f1: "01", f2: "已拍" }), row("r2", { f1: "02" })],
    );
    const s = summarizeTableSnapshot(prev, next);
    expect(s).toContain("新增 1 行");
    expect(s).toContain("修改 1 个单元格");
  });

  it("无变化 → 未改动", () => {
    const snap = tableSnap([field("f1", "镜号")], [row("r1", { f1: "01" })]);
    expect(summarizeTableSnapshot(snap, snap)).toBe("未改动");
  });

  it("损坏快照 → 空串（不崩）", () => {
    expect(summarizeTableSnapshot("not-json", "also-not-json")).toBe("");
  });
});

describe("cellValueEqual 图片值口径", () => {
  it("空图无模式 ≈ undefined（双向；序列化丢空/缺省键不误判为外部修改）", () => {
    expect(cellValueEqual(undefined, { images: [] })).toBe(true);
    expect(cellValueEqual({ images: [] }, undefined)).toBe(true);
  });

  it("空图 + display 差异 → 不等（九宫格偏好属值的一部分）", () => {
    expect(cellValueEqual({ images: [], display: "grid" }, { images: [] })).toBe(false);
    expect(cellValueEqual({ images: [], display: "grid" }, undefined)).toBe(false);
  });

  it("旧形态数组与新形态对象同内容 → 相等", () => {
    const legacy = ["img-1.png"] as unknown as CellValue; // 磁盘旧形态在运行时被标注为 CellValue
    expect(cellValueEqual(legacy, { images: ["img-1.png"] })).toBe(true);
  });
});

describe("图片值归一化", () => {
  it("normalizeTableRow：旧形态 string[] → { images }；新形态/无图片值行原引用保留（零拷贝）", () => {
    const oldRow = { id: "r1", values: { f1: ["img-1.png", "img-2.png"] } } as unknown as TableRow;
    expect(normalizeTableRow(oldRow).values.f1).toEqual({ images: ["img-1.png", "img-2.png"] });
    const newRow: TableRow = { id: "r2", values: { f1: { images: ["img-1.png"] } } };
    expect(normalizeTableRow(newRow)).toBe(newRow);
    const plain: TableRow = { id: "r3", values: { f2: "文本" } };
    expect(normalizeTableRow(plain)).toBe(plain);
  });
});

describe("selectionRegion（选中区域归约）", () => {
  const fields = [field("f1", "镜号"), field("f2", "景别"), field("f3", "备注")];
  const rows = [row("r1", { f1: "01" }), row("r2", { f1: "02" }), row("r3", { f1: "03" })];

  it("单格 = 单点", () => {
    expect(selectionRegion({ kind: "cell", rowId: "r2", fieldId: "f2" }, fields, rows)).toEqual({
      rowStart: 1,
      rowEnd: 1,
      colStart: 1,
      colEnd: 1,
    });
  });

  it("框选 = 两端点行/列 min/max（反向拖拽同样归一）", () => {
    expect(
      selectionRegion(
        { kind: "range", anchorRowId: "r3", anchorFieldId: "f3", rowId: "r1", fieldId: "f1" },
        fields,
        rows,
      ),
    ).toEqual({ rowStart: 0, rowEnd: 2, colStart: 0, colEnd: 2 });
  });

  it("整行 = 该行全列 / 整列 = 全行该列 / 整表 = 全表", () => {
    expect(selectionRegion({ kind: "row", rowId: "r2" }, fields, rows)).toEqual({
      rowStart: 1,
      rowEnd: 1,
      colStart: 0,
      colEnd: 2,
    });
    expect(selectionRegion({ kind: "column", fieldId: "f2" }, fields, rows)).toEqual({
      rowStart: 0,
      rowEnd: 2,
      colStart: 1,
      colEnd: 1,
    });
    expect(selectionRegion({ kind: "all" }, fields, rows)).toEqual({
      rowStart: 0,
      rowEnd: 2,
      colStart: 0,
      colEnd: 2,
    });
  });

  it("null / 画布 node 选中 / 失效 id → null", () => {
    expect(selectionRegion(null, fields, rows)).toBeNull();
    expect(selectionRegion({ kind: "node", nodeId: "n1" }, fields, rows)).toBeNull();
    expect(selectionRegion({ kind: "cell", rowId: "nope", fieldId: "f1" }, fields, rows)).toBeNull();
    expect(
      selectionRegion(
        { kind: "range", anchorRowId: "r1", anchorFieldId: "f1", rowId: "r2", fieldId: "nope" },
        fields,
        rows,
      ),
    ).toBeNull();
  });
});

describe("parseTsv / buildRegionTsv / cellToClipboardText（剪贴板 TSV）", () => {
  it("parseTsv：\\r\\n 归一 + 去尾空行 + 制表符分列", () => {
    expect(parseTsv("a\tb\r\nc\td\n")).toEqual([
      ["a", "b"],
      ["c", "d"],
    ]);
    expect(parseTsv("单格")).toEqual([["单格"]]);
  });

  it("cellToClipboardText：text/number 原样，image/空 = 空串", () => {
    expect(cellToClipboardText("hi")).toBe("hi");
    expect(cellToClipboardText(42)).toBe("42");
    expect(cellToClipboardText(undefined)).toBe("");
    expect(cellToClipboardText({ images: ["a.png"] })).toBe("");
  });

  it("buildRegionTsv：单格原值（多行无损）/ 多格制表换行拼接 / 值内嵌换行压空格", () => {
    const fields = [field("f1", "A"), field("f2", "B")];
    const rows = [row("r1", { f1: "x", f2: "y" }), row("r2", { f1: "多\n行", f2: "z" })];
    expect(buildRegionTsv(fields, rows, { rowStart: 0, rowEnd: 0, colStart: 0, colEnd: 0 })).toBe("x");
    expect(buildRegionTsv(fields, rows, { rowStart: 0, rowEnd: 1, colStart: 0, colEnd: 1 })).toBe(
      "x\ty\n多 行\tz",
    );
  });
});

describe("applyPasteGrid（粘贴网格回写）", () => {
  const numberField: TableField = { id: "f2", name: "数量", type: "number" };
  const rowsNum: TableRow[] = [{ id: "r1", values: { f1: "旧", f2: 1 } }];

  it("锚点回写 + 越界自动补行/补列（新列 text 字段「字段N」）", () => {
    const { fields: nf, rows: nr } = applyPasteGrid(
      [field("f1", "名称")],
      [row("r1", { f1: "a" })],
      0,
      0,
      [
        ["b", "c"],
        ["d", "e"],
      ],
    );
    expect(nf).toHaveLength(2);
    expect(nf[1]).toMatchObject({ type: "text", name: "字段2" });
    expect(nr).toHaveLength(2);
    expect(nr[0].values.f1).toBe("b");
    expect(nr[0].values[nf[1].id]).toBe("c");
    expect(nr[1].values.f1).toBe("d");
    expect(nr[1].values[nf[1].id]).toBe("e");
  });

  it("数字字段值强转（数字 / 非数字清空 / 空格清空）", () => {
    const { rows: nr } = applyPasteGrid([numberField], rowsNum, 0, 0, [["42"]]);
    expect(nr[0].values.f2).toBe(42);
    const { rows: nr2 } = applyPasteGrid([numberField], rowsNum, 0, 0, [["abc"]]);
    expect(nr2[0].values.f2).toBeUndefined();
    const { rows: nr3 } = applyPasteGrid([numberField], rowsNum, 0, 0, [[""]]);
    expect(nr3[0].values.f2).toBeUndefined();
  });

  it("无实际变化 → 原引用（不置脏不入栈）", () => {
    const fields = [field("f1", "名称"), numberField];
    const { fields: nf, rows: nr } = applyPasteGrid(fields, rowsNum, 0, 0, [["旧"]]);
    expect(nf).toBe(fields);
    expect(nr).toBe(rowsNum);
  });

  it("singleSelect：命中选项写入 / 非选项跳过（不覆盖）/ 空清空", () => {
    const sel: TableField = { id: "f1", name: "状态", type: "singleSelect", options: ["待拍", "已拍"] };
    const base: TableRow[] = [{ id: "r1", values: { f1: "待拍" } }];
    const { rows: hit } = applyPasteGrid([sel], base, 0, 0, [["已拍"]]);
    expect(hit[0].values.f1).toBe("已拍");
    const { rows: miss } = applyPasteGrid([sel], base, 0, 0, [["不存在"]]);
    expect(miss[0].values.f1).toBe("待拍");
    const { rows: cleared } = applyPasteGrid([sel], base, 0, 0, [[""]]);
    expect(cleared[0].values.f1).toBeUndefined();
  });

  it("image 字段跳过（不写入不清空）", () => {
    const img: TableField = { id: "f1", name: "图", type: "image" };
    const base: TableRow[] = [{ id: "r1", values: { f1: { images: ["a.png"] } } }];
    const { rows: nr } = applyPasteGrid([img], base, 0, 0, [["文本"]]);
    expect(nr[0].values.f1).toEqual({ images: ["a.png"] });
  });

  it("锚点偏移粘贴（从中间列开始，不碰锚点前的格）", () => {
    const { rows: nr } = applyPasteGrid(
      [field("f1", "A"), field("f2", "B")],
      [
        { id: "r1", values: { f1: "x", f2: "y" } },
        { id: "r2", values: {} },
      ] as TableRow[],
      0,
      1,
      [["z"]],
    );
    expect(nr[0].values.f1).toBe("x");
    expect(nr[0].values.f2).toBe("z");
    expect(nr[1].values.f2).toBeUndefined();
  });

  it("非有限数（Infinity/1e999）→ undefined（防落盘变 null 丢值）", () => {
    const { rows: nr } = applyPasteGrid([numberField], rowsNum, 0, 0, [["1e999"]]);
    expect(nr[0].values.f2).toBeUndefined();
    const { rows: nr2 } = applyPasteGrid([numberField], rowsNum, 0, 0, [["Infinity"]]);
    expect(nr2[0].values.f2).toBeUndefined();
  });

  it("网格全部被跳过（singleSelect 非选项）不补空行/空列", () => {
    const sel: TableField = { id: "f1", name: "状态", type: "singleSelect", options: ["待拍", "已拍"] };
    const base: TableRow[] = [{ id: "r1", values: { f1: "待拍" } }];
    const { fields, rows } = applyPasteGrid([sel], base, 1, 0, [["X"], ["Y"]]);
    expect(fields).toHaveLength(1);
    expect(rows).toHaveLength(1);
  });

  it("粘贴进空表（0 行 0 列）补行补列", () => {
    const { fields, rows } = applyPasteGrid([], [], 0, 0, [["a", "b"]]);
    expect(fields).toHaveLength(2);
    expect(rows).toHaveLength(1);
    expect(rows[0].values[fields[0].id]).toBe("a");
    expect(rows[0].values[fields[1].id]).toBe("b");
  });

  it("参差网格（短行不越界、不写空位）", () => {
    const { rows: nr } = applyPasteGrid(
      [field("f1", "A"), field("f2", "B")],
      [{ id: "r1", values: { f1: "x", f2: "y" } }] as TableRow[],
      0,
      0,
      [["p"], ["q", "r"]],
    );
    expect(nr[0].values.f1).toBe("p");
    expect(nr[0].values.f2).toBe("y"); // 短行无第 2 列，不写不覆盖
    expect(nr[1].values.f1).toBe("q");
    expect(nr[1].values.f2).toBe("r");
  });
});

describe("buildPluginTableSnapshot", () => {
  const peer = (id: number, color: string, presence: CollabPeer["presence"]): CollabPeer => ({
    peerId: id,
    nickname: `用户${id}`,
    color,
    deviceName: "设备",
    presence,
  });
  const cells = [field("f1", "镜号"), field("f2", "状态")];
  const rowsT: TableRow[] = [
    { id: "r1", values: { f1: "01" } },
    { id: "r2", values: { f1: "02" } },
    { id: "r3", values: { f1: "03" } },
  ];

  it("无表格：快照透传 null，不染任何行", () => {
    const peers = [peer(1, "#ff0000", { file: "a.atb", selection: { kind: "row", rowId: "r1" }, view: "table" })];
    const snap = buildPluginTableSnapshot(null, cells, rowsT, "r1", peers);
    expect(snap.tableFile).toBeNull();
    expect(snap.peerColorByRowId).toEqual({});
    expect(snap.selectedRowId).toBe("r1");
  });

  it("cell/range 选中染对应行（首个匹配优先）", () => {
    const p1 = peer(1, "#ff0000", { file: "a.atb", selection: { kind: "cell", rowId: "r2", fieldId: "f1" }, view: "table" });
    const p2 = peer(2, "#00ff00", { file: "a.atb", selection: { kind: "range", anchorRowId: "r1", anchorFieldId: "f1", rowId: "r3", fieldId: "f2" }, view: "table" });
    const snap = buildPluginTableSnapshot("a.atb", cells, rowsT, null, [p1, p2]);
    // p1（cell 单点）先染 r2；p2 range 覆盖 r1–r3，仅未染的 r1/r3 落 p2 色（r2 保持 p1 色）
    expect(snap.peerColorByRowId).toEqual({ r1: "#00ff00", r2: "#ff0000", r3: "#00ff00" });
  });

  it("column/all 选中不染；presence.file 不匹配忽略", () => {
    const peers = [
      peer(1, "#ff0000", { file: "a.atb", selection: { kind: "column", fieldId: "f1" }, view: "table" }),
      peer(2, "#00ff00", { file: "a.atb", selection: { kind: "all" }, view: "table" }),
      peer(3, "#0000ff", { file: "other.atb", selection: { kind: "row", rowId: "r1" }, view: "table" }),
    ];
    const snap = buildPluginTableSnapshot("a.atb", cells, rowsT, null, peers);
    expect(snap.peerColorByRowId).toEqual({});
  });

  it("远端选中引用已删除/不存在的行不染（region=null 跳过）", () => {
    const peers = [peer(1, "#ff0000", { file: "a.atb", selection: { kind: "row", rowId: "ghost" }, view: "table" })];
    const snap = buildPluginTableSnapshot("a.atb", cells, rowsT, null, peers);
    expect(snap.peerColorByRowId).toEqual({});
  });

  it("字段/行/选中原样透传（引用相等）", () => {
    const snap = buildPluginTableSnapshot("a.atb", cells, rowsT, "r2", []);
    expect(snap.fields).toBe(cells);
    expect(snap.rows).toBe(rowsT);
    expect(snap.selectedRowId).toBe("r2");
    expect(snap.tableFile).toBe("a.atb");
  });
});
