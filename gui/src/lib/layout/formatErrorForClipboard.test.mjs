import test from "node:test";
import assert from "node:assert/strict";

// Mirror of formatErrorForClipboard.ts for node:test without a TypeScript loader.
function formatErrorForClipboard(diagnostic) {
  return `${diagnostic.code}: ${diagnostic.message}
profile: ${diagnostic.profile ?? "-"}
module: ${diagnostic.module_id ?? "-"}
property: ${diagnostic.property_path ?? "-"}
reason: ${diagnostic.reason}
fix: ${diagnostic.fix}`;
}

test("formats a bridge-violation diagnostic for a disconnected chatbot", () => {
  const diagnostic = {
    code: "TWLAYOUT-E032",
    severity: "error",
    message: "Bridge span is not allowed for this profile",
    file: null,
    line: null,
    column: null,
    profile: "rectangular",
    module_id: "media-1",
    property_path: "span_bridge",
    reason: "The rectangular surface has no bridge region.",
    fix: "Disable span_bridge or choose a curved profile that permits media-only spanning.",
  };

  assert.equal(
    formatErrorForClipboard(diagnostic),
    `TWLAYOUT-E032: Bridge span is not allowed for this profile
profile: rectangular
module: media-1
property: span_bridge
reason: The rectangular surface has no bridge region.
fix: Disable span_bridge or choose a curved profile that permits media-only spanning.`,
  );
});
