import { test } from "node:test";
import assert from "node:assert/strict";
import { adjacentIssue, nextUnreviewed, acceptsReviewShortcut } from "../src/utils/review-navigation.ts";

test("navigation starts at the right edge after filtering out the previous selection", () => {
  const issues = [{ id: "a" }, { id: "b" }, { id: "c" }];
  assert.equal(adjacentIssue(issues, "hidden", -1).id, "c");
  assert.equal(adjacentIssue(issues, "hidden", 1).id, "a");
  assert.equal(adjacentIssue(issues, "a", -1).id, "c");
  assert.equal(adjacentIssue([], null, 1), undefined);
});

test("next unreviewed skips feedback and wraps without selecting the current issue", () => {
  const issues = [{ id: "a" }, { id: "b", feedback: "correct" }, { id: "c" }];
  assert.equal(nextUnreviewed(issues, "a").id, "c");
  assert.equal(nextUnreviewed(issues, "c").id, "a");
  assert.equal(nextUnreviewed([{ id: "a" }], "a"), undefined);
  assert.equal(nextUnreviewed([], null), undefined);
});

test("review shortcuts leave typing, editing and browser shortcuts alone", () => {
  const event = { target: null, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, repeat: false, isComposing: false };
  assert.equal(acceptsReviewShortcut(event), true);
  for (const key of ["ctrlKey", "metaKey", "altKey", "shiftKey", "repeat", "isComposing"]) assert.equal(acceptsReviewShortcut({ ...event, [key]: true }), false);
  assert.equal(acceptsReviewShortcut({ ...event, target: { closest: () => ({}) } }), false);
});
