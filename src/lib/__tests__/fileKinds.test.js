import { describe, expect, it } from "vitest";

import { fileExtension, fileKind, fileKindKey } from "../fileKinds";

describe("fileKind", () => {
  it("recognizes office, document and archive formats", () => {
    for (const name of ["a.doc", "a.docs", "a.docx", "a.odt"]) {
      expect(fileKind({ name })).toBe("word");
    }
    for (const name of ["a.xls", "a.xlsx", "a.csv", "a.ods"]) {
      expect(fileKind({ name })).toBe("spreadsheet");
    }
    for (const name of ["a.ppt", "a.pptx", "a.odp"]) {
      expect(fileKind({ name })).toBe("presentation");
    }
    expect(fileKind({ name: "a.pdf" })).toBe("pdf");
    expect(fileKind({ name: "a.zip" })).toBe("archive");
  });

  it("recognizes common code and text extensions", () => {
    for (const name of [
      "a.js",
      "a.ts",
      "a.vue",
      "a.html",
      "a.htm",
      "a.css",
      "a.json",
      "a.jsonc",
      "a.php",
      "a.py",
      "a.go",
      "a.rs",
      "a.c",
      "a.cpp",
      "a.c++",
    ]) {
      expect(fileKind({ name })).toBe("code");
    }
    expect(fileKind({ name: "a.md" })).toBe("text");
    expect(fileKind({ name: "a.txt" })).toBe("text");
  });

  it("prefers content type and falls back to a generic file", () => {
    expect(
      fileKind({ name: "download", contentType: "application/pdf" }),
    ).toBe("pdf");
    expect(fileKind({ name: "unknown.bin", contentType: null })).toBe("file");
    expect(fileKind({ name: "folder", isDir: true })).toBe("folder");
    expect(fileExtension("A.DOCX")).toBe("docx");
    expect(fileKindKey("spreadsheet")).toBe("peer.spreadsheet");
  });
});