import type { SourceLocation } from "../api/types";

export function formatSourceLocation(location: SourceLocation): string {
  if (location.page) {
    const start = location.line_start ?? 1;
    const end = location.line_end ?? start;
    if (location.page_end && location.page_end !== location.page) {
      return `第 ${location.page} 页第 ${start} 行至第 ${location.page_end} 页第 ${end} 行`;
    }
    if (location.page_end === undefined && end < start) {
      return `第 ${location.page} 页第 ${start} 行起（结束页未记录）`;
    }
    return start === end
      ? `第 ${location.page} 页，第 ${start} 行`
      : `第 ${location.page} 页，第 ${start}–${end} 行`;
  }
  if (location.line_start) {
    const end = location.line_end ?? location.line_start;
    return end === location.line_start
      ? `第 ${location.line_start} 行`
      : `第 ${location.line_start}–${end} 行`;
  }
  if (location.paragraph_index !== undefined) {
    const paragraph = location.paragraph_index + 1;
    return location.sentence_index === undefined
      ? `第 ${paragraph} 段`
      : `第 ${paragraph} 段，第 ${location.sentence_index + 1} 句`;
  }
  return `字符 ${location.char_start}–${location.char_end}`;
}
