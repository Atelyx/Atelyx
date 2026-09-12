/**
 * 附件引用形态判定（utils/tempAttachmentPath.ts，纯函数）。
 * 未入库状态只由引用形态判定（不再另存标记字段），判定错会让「保存到仓库」入口与回收范围出错。
 */
import { describe, it, expect } from "vitest";
import { TEMP_ATTACHMENT_DIR, isTempAttachmentRef } from "./tempAttachmentPath";

describe("isTempAttachmentRef", () => {
  it("临时区前缀命中（与后端常量同值）", () => {
    expect(TEMP_ATTACHMENT_DIR).toBe(".atelyx/temp");
    expect(isTempAttachmentRef(".atelyx/temp/0123456789abcdef/att-x-图 1.png")).toBe(true);
  });

  it("仓库附件路径与空值不命中", () => {
    expect(isTempAttachmentRef("附件/x.png")).toBe(false);
    expect(isTempAttachmentRef(".atelyx/attachments/t1/x.png")).toBe(false);
    // 仅前缀相同但不在该目录下（不得误判）
    expect(isTempAttachmentRef(".atelyx/tempX/a.png")).toBe(false);
    expect(isTempAttachmentRef(undefined)).toBe(false);
    expect(isTempAttachmentRef(null)).toBe(false);
    expect(isTempAttachmentRef("")).toBe(false);
  });
});
