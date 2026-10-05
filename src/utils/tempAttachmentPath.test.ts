/**
 * 附件引用形态判定（utils/tempAttachmentPath.ts，纯函数）。
 * 未入库状态只由引用形态判定（不再另存标记字段），判定错会让「保存到仓库」入口与回收范围出错。
 */
import { describe, it, expect } from "vitest";
import { TEMP_ATTACHMENT_DIR, isTempAttachmentRef, tempInstanceKey } from "./tempAttachmentPath";

describe("isTempAttachmentRef", () => {
  it("临时区前缀命中（与后端常量同值）", () => {
    expect(TEMP_ATTACHMENT_DIR).toBe(".atelyx/temp");
    expect(isTempAttachmentRef(".atelyx/temp/0123456789abcdef/att-x-图 1.png")).toBe(true);
  });

  it("仓库附件路径与空值不命中", () => {
    expect(isTempAttachmentRef("附件/x.png")).toBe(false);
    expect(isTempAttachmentRef(".atelyx/history/x.png")).toBe(false);
    // 仅前缀相同但不在该目录下（不得误判）
    expect(isTempAttachmentRef(".atelyx/tempX/a.png")).toBe(false);
    expect(isTempAttachmentRef(undefined)).toBe(false);
    expect(isTempAttachmentRef(null)).toBe(false);
    expect(isTempAttachmentRef("")).toBe(false);
  });
});

describe("tempInstanceKey", () => {
  it("FNV-1a 64 标准测试向量（与 Rust 侧同算法，跨语言一致性钉死）", () => {
    // FNV-1a 64 公开向量：空串 = offset basis；"a" 与 "foobar" 为标准参考值
    expect(tempInstanceKey("")).toBe("cbf29ce484222325");
    expect(tempInstanceKey("a")).toBe("af63dc4c8601ec8c");
    expect(tempInstanceKey("foobar")).toBe("85944171f73967e8");
  });

  it("输出恒为 16 位小写十六进制（不含路径语义），同 id 恒同 key", () => {
    for (const id of ["x", "uuid-4f2c-...", "中文 id", "../../etc", "C:\\Windows"]) {
      const key = tempInstanceKey(id);
      expect(key).toMatch(/^[0-9a-f]{16}$/);
      expect(tempInstanceKey(id)).toBe(key);
    }
    expect(tempInstanceKey("x")).not.toBe(tempInstanceKey("y"));
  });
});
