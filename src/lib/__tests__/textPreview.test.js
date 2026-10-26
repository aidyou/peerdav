import { describe, expect, it } from "vitest";

import {
  parseDelimited,
  previewFormat,
  tableDelimiter,
} from "../textPreview";
import { highlightCode, renderMarkdown } from "../textPreviewRender";

describe("previewFormat", () => {
  it("selects markdown, table, code and plain text renderers", () => {
    expect(previewFormat({ name: "README.md" })).toBe("markdown");
    expect(
      previewFormat({ name: "notes", contentType: "text/markdown" }),
    ).toBe("markdown");
    expect(previewFormat({ name: "data.csv" })).toBe("table");
    expect(previewFormat({ name: "data.tsv" })).toBe("table");
    expect(previewFormat({ name: "main.rs" })).toBe("code");
    expect(previewFormat({ name: "notes.txt" })).toBe("text");
  });

  it("uses the matching delimiter for CSV and TSV", () => {
    expect(tableDelimiter({ name: "data.csv" })).toBe(",");
    expect(tableDelimiter({ name: "data.tsv" })).toBe("\t");
    expect(
      tableDelimiter({ name: "data", contentType: "text/tab-separated-values" }),
    ).toBe("\t");
  });
});

describe("parseDelimited", () => {
  it("parses quoted fields, escaped quotes and multiline values", () => {
    const parsed = parseDelimited(
      'name,note\r\n"Ada","hello, world"\r\n"Lin","line 1\nline 2"\r\n"Q","a ""quote"""\r\n',
    );
    expect(parsed.header).toEqual(["name", "note"]);
    expect(parsed.rows).toEqual([
      ["Ada", "hello, world"],
      ["Lin", "line 1\nline 2"],
      ["Q", 'a "quote"'],
    ]);
    expect(parsed.columnCount).toBe(2);
  });

  it("parses TSV and pads ragged rows by column count", () => {
    const parsed = parseDelimited("a\tb\tc\n1\t2\n", "\t");
    expect(parsed.header).toEqual(["a", "b", "c"]);
    expect(parsed.rows).toEqual([["1", "2"]]);
    expect(parsed.columnCount).toBe(3);
  });

  it("strips a UTF-8 BOM and keeps an empty final field", () => {
    const parsed = parseDelimited("\uFEFFa,b,\n1,2,");
    expect(parsed.header).toEqual(["a", "b", ""]);
    expect(parsed.rows).toEqual([["1", "2", ""]]);
  });
});

describe("renderMarkdown", () => {
  it("renders headings and safe external links", () => {
    const html = renderMarkdown("# Title\n\n[Example](https://example.com)");
    expect(html).toContain("<h1>Title</h1>");
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("does not execute raw HTML or javascript link targets", () => {
    const html = renderMarkdown(
      '<script>alert("x")</script>\n\n[bad](javascript:alert(1))',
    );
    expect(html).not.toContain("<script>");
    expect(html).not.toContain('href="javascript:');
  });
});

describe("highlightCode", () => {
  it("highlights supported languages and escapes unknown sources", () => {
    const rust = highlightCode("fn main() {}", { name: "main.rs" });
    expect(rust.language).toBe("rust");
    expect(rust.html).toContain("hljs-keyword");

    const unknown = highlightCode("<script>alert(1)</script>", {
      name: "file.unknown",
    });
    expect(unknown.language).toBe("plaintext");
    expect(unknown.html).toBe("&lt;script&gt;alert(1)&lt;/script&gt;");
  });
});