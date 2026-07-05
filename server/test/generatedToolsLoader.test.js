// Loader unit tests (issue #134, docs/SELF-EXTENSION-V2.md T2 row). Fixture
// tool files are written under server/test/ (not os.tmpdir()) so Node's
// module resolution walks up to server/node_modules and 'zod' resolves - the
// same reason worktrees (nested under repoRoot) can dynamic-import their own
// generated files in production.
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadGeneratedTools, validateToolShape } from "../agent/tools/generated/index.js";

let fixtureDir;

async function writeFixture(name, source) {
  await fs.writeFile(path.join(fixtureDir, name), source, "utf8");
}

const VALID_TOOL = [
  'import { z } from "zod";',
  "",
  "const inputSchema = z.object({ tradeId: z.string() });",
  "",
  "export default {",
  '  name: "rMultiple",',
  '  description: "Compute the R-multiple for a closed trade entity.",',
  '  classification: "allowed",',
  "  inputSchema,",
  '  example: { tradeId: "ent_trade_1" },',
  "  request: ({ args }) => ({ method: \"GET\", path: `/api/entity/${args.tradeId}` }),",
  "};",
  "",
].join("\n");

const GATED_TOOL = [
  'import { z } from "zod";',
  "",
  "const inputSchema = z.object({ text: z.string() });",
  "",
  "export default {",
  '  name: "draftSummary",',
  '  description: "Draft a summary for human approval.",',
  '  classification: "gated",',
  "  inputSchema,",
  '  example: { text: "hello" },',
  '  request: ({ args }) => ({ method: "POST", path: "/api/entity", body: { attrs: args } }),',
  "};",
  "",
].join("\n");

beforeEach(async () => {
  fixtureDir = await fs.mkdtemp(path.join(path.resolve(import.meta.dirname), ".tmp-generated-tools-"));
});

afterEach(async () => {
  await fs.rm(fixtureDir, { recursive: true, force: true });
});

describe("validateToolShape", () => {
  it("accepts a well-formed default export", async () => {
    await writeFixture("ok.js", VALID_TOOL);
    const mod = await import(`${fixtureDir}/ok.js`);
    const result = validateToolShape(mod);
    expect(result.valid).toBe(true);
    expect(result.tool.name).toBe("rMultiple");
  });

  it("rejects a module with no default export", () => {
    const result = validateToolShape({});
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/no default export/);
  });

  it("rejects a missing 'example' field", () => {
    const result = validateToolShape({
      default: {
        name: "x",
        description: "d",
        classification: "allowed",
        inputSchema: { safeParse: () => {}, parse: () => {} },
        request: () => ({}),
      },
    });
    expect(result.valid).toBe(false);
    expect(result.errors.join(" ")).toMatch(/example/);
  });
});

describe("loadGeneratedTools", () => {
  it("merges a valid tool with correct classification/description/inputSchema/requestFn", async () => {
    await writeFixture("rMultiple.js", VALID_TOOL);

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(warnings).toEqual([]);
    expect(tools.rMultiple).toBeTruthy();
    expect(tools.rMultiple.classification).toBe("allowed");
    expect(tools.rMultiple.description).toMatch(/R-multiple/);
    expect(typeof tools.rMultiple.requestFn).toBe("function");
    expect(tools.rMultiple.generated).toBe(true);
    // inputSchema is the raw zod shape (for the SDK's tool() helper), not the
    // full zod object - matches the static REGISTRY entries' convention.
    expect(tools.rMultiple.inputSchema.tradeId).toBeTruthy();
  });

  it("retrievable via a classify-equivalent lookup once merged into a registry", async () => {
    await writeFixture("rMultiple.js", VALID_TOOL);
    const { tools } = await loadGeneratedTools({ dir: fixtureDir });

    const merged = { "entity.create": { classification: "allowed" }, ...tools };
    const classifyFrom = (registry, name) => registry[name]?.classification ?? "forbidden";

    expect(classifyFrom(merged, "rMultiple")).toBe("allowed");
    expect(classifyFrom(merged, "entity.create")).toBe("allowed");
    expect(classifyFrom(merged, "unknown.tool")).toBe("forbidden");
  });

  it("skips a bad-shape file with a warning instead of crashing the loop", async () => {
    await writeFixture("good.js", VALID_TOOL);
    await writeFixture("bad.js", "export default { name: 42 };\n");

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(tools.rMultiple).toBeTruthy();
    expect(warnings.some((w) => w.includes("bad.js"))).toBe(true);
  });

  it("skips a file that throws on import (syntax/runtime error) with a warning", async () => {
    await writeFixture("broken.js", "this is not valid javascript {{{\n");

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(Object.keys(tools)).toEqual([]);
    expect(warnings.some((w) => w.includes("broken.js"))).toBe(true);
  });

  it("rejects a name collision with an existing/protected tool", async () => {
    await writeFixture("shadow.js", VALID_TOOL.replace('name: "rMultiple"', 'name: "entity.create"'));

    const { tools, warnings } = await loadGeneratedTools({
      dir: fixtureDir,
      existingNames: new Set(["entity.create"]),
    });

    expect(tools["entity.create"]).toBeUndefined();
    expect(warnings.some((w) => w.includes("collides"))).toBe(true);
  });

  it("rejects a name collision between two generated files (first wins, second warned)", async () => {
    await writeFixture("a.js", VALID_TOOL);
    await writeFixture("b.js", VALID_TOOL.replace(/request:.*$/m, 'request: () => ({ method: "GET", path: "/api/entity" }),'));

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(Object.keys(tools)).toEqual(["rMultiple"]);
    expect(warnings.some((w) => w.includes("collides"))).toBe(true);
  });

  it("rejects a descriptor whose route is outside the allowlist at load time", async () => {
    const orderTool = VALID_TOOL.replace(
      'request: ({ args }) => ({ method: "GET", path: `/api/entity/${args.tradeId}` }),',
      'request: ({ args }) => ({ method: "POST", path: "/api/orders/place", body: args }),',
    );
    await writeFixture("badRoute.js", orderTool);

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(tools.rMultiple).toBeUndefined();
    expect(warnings.some((w) => w.includes("outside the allowlist"))).toBe(true);
  });

  it("rejects a descriptor whose example does not satisfy its own inputSchema", async () => {
    const badExample = VALID_TOOL.replace('example: { tradeId: "ent_trade_1" },', "example: { tradeId: 123 },");
    await writeFixture("badExample.js", badExample);

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(tools.rMultiple).toBeUndefined();
    expect(warnings.length).toBeGreaterThan(0);
  });

  it("loads a gated tool with classification preserved", async () => {
    await writeFixture("draftSummary.js", GATED_TOOL);

    const { tools } = await loadGeneratedTools({ dir: fixtureDir });

    expect(tools.draftSummary.classification).toBe("gated");
  });

  it("never throws on an unreadable directory - returns empty tools with a warning", async () => {
    const missingDir = path.join(os.tmpdir(), "lifeos-does-not-exist-" + Date.now());
    const { tools, warnings } = await loadGeneratedTools({ dir: missingDir });

    expect(tools).toEqual({});
    expect(warnings.length).toBeGreaterThan(0);
  });

  it("skips its own index.js file", async () => {
    await writeFixture("index.js", "export const noop = true;\n");
    await writeFixture("rMultiple.js", VALID_TOOL);

    const { tools, warnings } = await loadGeneratedTools({ dir: fixtureDir });

    expect(Object.keys(tools)).toEqual(["rMultiple"]);
    expect(warnings).toEqual([]);
  });
});
