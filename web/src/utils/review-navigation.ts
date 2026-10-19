export function adjacentIssue<T extends { id: string }>(issues: T[], currentId: string | null, direction: -1 | 1): T | undefined {
  const index = issues.findIndex((issue) => issue.id === currentId);
  const next = index < 0 ? (direction === 1 ? 0 : issues.length - 1) : (index + direction + issues.length) % issues.length;
  return issues[next];
}

export function nextUnreviewed<T extends { id: string; feedback?: string }>(issues: T[], currentId: string | null): T | undefined {
  const start = issues.findIndex((issue) => issue.id === currentId);
  for (let step = 1; step <= issues.length; step += 1) {
    const issue = issues[(start + step) % issues.length];
    if (issue && !issue.feedback && issue.id !== currentId) return issue;
  }
  return undefined;
}

export function acceptsReviewShortcut(event: Pick<KeyboardEvent, "target" | "ctrlKey" | "altKey" | "metaKey" | "shiftKey" | "isComposing" | "repeat">): boolean {
  if (event.ctrlKey || event.altKey || event.metaKey || event.shiftKey || event.isComposing || event.repeat) return false;
  const target = event.target as HTMLElement | null;
  return !target?.closest?.("input, textarea, select, button, a, [contenteditable='true'], [role='textbox']");
}
