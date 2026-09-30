import { test } from "node:test";
import assert from "node:assert/strict";
import { formatSourceLocation } from "../src/utils/source-location.ts";

const base = { document_format: "pdf", char_start: 0, char_end: 2, quote: "测试" };

test("cross-page range displays the end page even when its line number is smaller", () => {
  assert.equal(formatSourceLocation({ ...base, page: 1, page_end: 2, line_start: 50, line_end: 3 }), "第 1 页第 50 行至第 2 页第 3 行");
});
test("legacy reversed line ranges do not invent an end page", () => {
  assert.equal(formatSourceLocation({ ...base, page: 1, line_start: 50, line_end: 3 }), "第 1 页第 50 行起（结束页未记录）");
});
test("same-page ranges and single lines remain readable", () => {
  assert.equal(formatSourceLocation({ ...base, page: 1, line_start: 2, line_end: 3 }), "第 1 页，第 2–3 行");
  assert.equal(formatSourceLocation({ ...base, page: 1, line_start: 2, line_end: 2 }), "第 1 页，第 2 行");
});
test("TXT locations use original line numbers", () => {
  assert.equal(formatSourceLocation({ ...base, document_format: "txt", line_start: 3, line_end: 4 }), "第 3–4 行");
});
test("DOCX locations use paragraph and sentence numbers", () => {
  assert.equal(formatSourceLocation({ ...base, document_format: "docx", paragraph_index: 1, sentence_index: 2 }), "第 2 段，第 3 句");
});
test("locations without source coordinates fall back to the character range", () => {
  assert.equal(formatSourceLocation(base), "字符 0–2");
});
