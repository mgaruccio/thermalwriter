import test from "node:test";
import assert from "node:assert/strict";

// Mirror of asyncSelection.ts for node:test without a TS loader.
function bumpRevision(current) {
  return current + 1;
}
function isCurrentRevision(expected, current) {
  return expected === current;
}

test("stale layout selection results are discarded", () => {
  let rev = 0;
  const first = bumpRevision(rev);
  rev = first;
  const second = bumpRevision(rev);
  rev = second;
  assert.equal(isCurrentRevision(first, rev), false);
  assert.equal(isCurrentRevision(second, rev), true);
});
