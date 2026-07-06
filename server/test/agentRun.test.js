// CLI arg contract for `agent/run.js` (issue #144): the `--dry-run` flag that
// makes the daily-brief turn read-only. Only the pure parser is under test -
// the process entry itself shells `runAgentTurn`, covered by the loop suite.
import { describe, expect, it } from "vitest";
import { parseArgs } from "../agent/run.js";

describe("parseArgs", () => {
  it("parses prompt and workspaceId positionally with dryRun defaulting false", () => {
    expect(parseArgs(["what is due", "ws1"])).toEqual({
      prompt: "what is due",
      workspaceId: "ws1",
      dryRun: false,
    });
  });

  it("sets dryRun when --dry-run is present, anywhere in the args", () => {
    expect(parseArgs(["--dry-run", "brief me", "ws1"])).toEqual({
      prompt: "brief me",
      workspaceId: "ws1",
      dryRun: true,
    });
    expect(parseArgs(["brief me", "ws1", "--dry-run"])).toEqual({
      prompt: "brief me",
      workspaceId: "ws1",
      dryRun: true,
    });
  });

  it("leaves prompt/workspaceId undefined when missing (caller errors out)", () => {
    expect(parseArgs([])).toEqual({ prompt: undefined, workspaceId: undefined, dryRun: false });
  });
});
