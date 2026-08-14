import type { LayoutDiagnostic } from "./types";

/** Render one backend diagnostic as a standalone, paste-ready error report. */
export function formatErrorForClipboard(diagnostic: LayoutDiagnostic): string {
  return `${diagnostic.code}: ${diagnostic.message}
profile: ${diagnostic.profile ?? "-"}
module: ${diagnostic.module_id ?? "-"}
property: ${diagnostic.property_path ?? "-"}
reason: ${diagnostic.reason}
fix: ${diagnostic.fix}`;
}
