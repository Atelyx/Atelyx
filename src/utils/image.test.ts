/**
 * 图片扩展名与 MIME 单一真源的契约测试。
 *
 * 锁定三件事：MIME 映射（含大小写与非图片返回 null）、文件名后缀判定（要求点号与完整扩展名）、
 * 文件选择器 accept 串的精确值——该串直接进 `<input accept>`，取值变化会改变可选文件范围。
 */
import { describe, expect, it } from "vitest";
import { IMAGE_ACCEPT, IMAGE_EXTS, imageMimeFromExt, isImageFileName } from "./image";

describe("imageMimeFromExt", () => {
  it("映射每个图片扩展名", () => {
    expect(imageMimeFromExt("png")).toBe("image/png");
    expect(imageMimeFromExt("jpg")).toBe("image/jpeg");
    expect(imageMimeFromExt("jpeg")).toBe("image/jpeg");
    expect(imageMimeFromExt("webp")).toBe("image/webp");
    expect(imageMimeFromExt("gif")).toBe("image/gif");
  });

  it("大小写不敏感", () => {
    expect(imageMimeFromExt("PNG")).toBe("image/png");
    expect(imageMimeFromExt("JpEg")).toBe("image/jpeg");
  });

  it("非图片返回 null（回落值由调用方决定）", () => {
    expect(imageMimeFromExt("svg")).toBeNull();
    expect(imageMimeFromExt("md")).toBeNull();
    expect(imageMimeFromExt("")).toBeNull();
  });
});

describe("isImageFileName", () => {
  it("命中结尾的图片扩展名，大小写不敏感", () => {
    expect(isImageFileName("a.png")).toBe(true);
    expect(isImageFileName("a.PNG")).toBe(true);
    expect(isImageFileName("目录/a.jpeg")).toBe(true);
    expect(isImageFileName("a.tmp.png")).toBe(true);
  });

  it("要求点号与完整扩展名", () => {
    expect(isImageFileName("png")).toBe(false);
    expect(isImageFileName("a.pn")).toBe(false);
    expect(isImageFileName("a.pngx")).toBe(false);
    expect(isImageFileName("a.png ")).toBe(false);
    expect(isImageFileName("a.svg")).toBe(false);
    expect(isImageFileName("a.")).toBe(false);
  });
});

describe("IMAGE_EXTS / IMAGE_ACCEPT", () => {
  it("扩展名集合每个只列一次", () => {
    expect(IMAGE_EXTS).toEqual(["png", "jpg", "jpeg", "webp", "gif"]);
  });

  it("accept 串与文件选择器既有取值逐字一致（jpg/jpeg 归并为一个 MIME）", () => {
    expect(IMAGE_ACCEPT).toBe("image/png,image/jpeg,image/webp,image/gif");
  });
});
