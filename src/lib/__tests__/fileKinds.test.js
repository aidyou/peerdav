import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import FileGlyph from "../../components/FileGlyph.vue";

import {
  canPreview,
  fileExtension,
  fileIconBadge,
  fileKind,
  fileKindKey,
  isReadableText,
} from "../fileKinds";

describe("fileKind", () => {
  it.each([
    ["image", "jpg jpeg png bmp gif svg webp avif heic tif tiff ico"],
    ["audio", "mp3 wav ogg m4a aac flac opus wma"],
    ["video", "mp4 mov mkv avi webm m4v wmv"],
    ["word", "doc docx docm dot dotx odt rtf pages"],
    ["spreadsheet", "xls xlsx xlsm xlsb csv tsv ods numbers"],
    ["presentation", "ppt pptx pptm pps ppsx potx odp key"],
    ["archive", "zip rar 7z tar gz tgz bz2 xz zst jar war"],
    ["database", "db sqlite sqlite3 mdb accdb dbf"],
    ["font", "ttf otf woff woff2 eot"],
    ["package", "exe msi msix dmg pkg deb rpm apk appimage iso"],
  ])("recognizes %s suffixes case-insensitively", (kind, suffixes) => {
    for (const suffix of suffixes.split(" ")) {
      expect(fileKind({ name: `sample.${suffix.toUpperCase()}` })).toBe(kind);
      expect(fileKindKey(kind)).not.toBe("peer.file");
    }
  });

  it.each([
    ["go", "GO"], ["rs", "RS"], ["php", "PHP"], ["js", "JS"], ["ts", "TS"],
    ["css", "CSS"], ["html", "HTML"], ["htm", "HTML"], ["shtml", "HTML"],
    ["lua", "LUA"], ["java", "JAVA"], ["c", "C"], ["cpp", "C++"], ["c++", "C++"],
    ["cs", "C#"], ["xml", "XML"], ["py", "PY"], ["rb", "RB"], ["vue", "VUE"],
    ["mts", "TS"], ["cts", "TS"], ["h++", "C++"], ["php8", "PHP"],
  ])("renders a distinct language badge for .%s", (suffix, label) => {
    const entry = { name: `source.${suffix.toUpperCase()}` };
    expect(fileKind(entry)).toBe("code");
    expect(isReadableText(entry)).toBe(true);
    expect(fileIconBadge(entry)?.label).toBe(label);
    const wrapper = mount(FileGlyph, { props: { entry } });
    expect(wrapper.find("svg text").text()).toBe(label);
    expect(wrapper.attributes("aria-hidden")).toBe("true");
    expect(wrapper.classes()).toContain(`file-glyph--${fileIconBadge(entry).tone}`);
    wrapper.unmount();
  });

  it("keeps folders, unknown files and binary formats safe", async () => {
    expect(fileIconBadge({ name: "folder.rs", isDir: true })).toBeNull();
    expect(fileIconBadge({ name: "image.js", contentType: "image/png" })).toBeNull();
    expect(fileIconBadge({ name: "unknown.xyz" })).toBeNull();
    expect(fileKind({ name: "main.ts", contentType: "video/mp2t" })).toBe("code");
    expect(fileKind({ name: "download", contentType: "APPLICATION/PDF; charset=binary" })).toBe("pdf");
    for (const name of ["app.jar", "font.woff2", "db.sqlite", "installer.exe"]) {
      expect(isReadableText({ name })).toBe(false);
      expect(canPreview({ name })).toBe(false);
    }
    const wrapper = mount(FileGlyph, { props: { entry: { name: "main.go" }, size: "xl" } });
    expect(wrapper.classes()).toContain("file-glyph--xl");
    for (const [name, kind] of [["song.mp3", "audio"], ["clip.mp4", "video"], ["db.sqlite", "database"], ["font.ttf", "font"], ["app.deb", "package"], ["folder.ts", "folder"], ["unknown.xyz", "file"]]) {
      await wrapper.setProps({ entry: { name, isDir: kind === "folder" } });
      expect(wrapper.classes()).toContain(`file-glyph--${kind}`);
      expect(wrapper.findAll("svg")).toHaveLength(1);
      expect(wrapper.find("svg text").exists()).toBe(false);
    }
    wrapper.unmount();
  });
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
