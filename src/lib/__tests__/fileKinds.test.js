import { describe, expect, it } from "vitest";

import {
  canPreview,
  fileExtension,
  fileKind,
  fileKindKey,
  isReadableText,
} from "../fileKinds";

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
      "a.vue",
      "a.yml",
      "a.toml",
      "a.htaccess",
    ]) {
      expect(fileKind({ name })).toBe("code");
    }
    expect(fileKind({ name: "a.md" })).toBe("text");
    expect(fileKind({ name: "a.txt" })).toBe("text");
    expect(fileKind({ name: "Dockerfile" })).toBe("text");
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

describe("isReadableText", () => {
  it("accepts text, code, data and content-type text entries", () => {
    for (const name of [
      "notes.txt",
      "README.md",
      "app.log",
      "config.yaml",
      "vite.config.mjs",
      "main.rs",
      "App.vue",
      "data.json",
      "table.csv",
      "settings.toml",
      "app.conf",
      ".env",
      ".htaccess",
      "Dockerfile",
    ]) {
      expect(isReadableText({ name })).toBe(true);
    }
  });

  it("does not trust a server-reported text MIME without a text suffix", () => {
    expect(
      isReadableText({ name: "blob", contentType: "text/plain; charset=utf-8" }),
    ).toBe(false);
  });

  it("rejects folders, images, office documents, archives and binaries", () => {
    for (const entry of [
      { name: "folder", isDir: true },
      { name: "photo.png", contentType: "image/png" },
      { name: "icon.svg", contentType: "image/svg+xml" },
      { name: "report.pdf", contentType: "application/pdf" },
      { name: "report.docx" },
      { name: "sheet.xlsx" },
      { name: "bundle.zip" },
      { name: "payload.bin", contentType: "application/octet-stream" },
    ]) {
      expect(isReadableText(entry)).toBe(false);
    }
  });
});

describe("canPreview", () => {
  it("allows text, images and playable media suffixes", () => {
    for (const entry of [
      { name: "README.md" },
      { name: "photo.png", contentType: "image/png" },
      { name: "track.mp3", contentType: "audio/mpeg" },
      { name: "clip.mp4", contentType: "video/mp4" },
      { name: ".htaccess" },
      { name: "Dockerfile" },
    ]) {
      expect(canPreview(entry)).toBe(true);
    }
  });

  it("keeps unknown and known binary formats out of preview", () => {
    for (const entry of [
      { name: "folder", isDir: true },
      { name: "report.pdf" },
      { name: "report.docx" },
      { name: "sheet.xlsx" },
      { name: "slides.pptx" },
      { name: "bundle.zip" },
      { name: "payload.custom" },
    ]) {
      expect(canPreview(entry)).toBe(false);
    }
  });
});
