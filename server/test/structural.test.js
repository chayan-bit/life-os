import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { validateStructural } from "../validators/structural.js";

const REAL_MODULES_DIR = path.resolve(import.meta.dirname, "..", "..", "modules");

let modulesDir;

async function writeModule(id, source) {
  const dir = path.join(modulesDir, id);
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(path.join(dir, "module.js"), source, "utf8");
  return path.join(dir, "module.js");
}

const VALID_SOURCE = `osRegisterModule({
  id: "widgets",
  name: "Widgets",
  icon: "Zap",
  color: "var(--neo-yellow)",
  entityTypes: {
    widget: {
      label: "Widget",
      plural: "Widgets",
      icon: "FileText",
      attrs: { name: { type: "text", required: true } },
    },
  },
  views: [{ id: "all", label: "All", kind: "list", type: "widget" }],
});`;

beforeEach(async () => {
  modulesDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-structural-modules-"));
});

afterEach(async () => {
  await fs.rm(modulesDir, { recursive: true, force: true });
});

describe("validateStructural - happy path", () => {
  it("passes a well-formed manifest with no siblings", async () => {
    const modulePath = await writeModule("widgets", VALID_SOURCE);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result).toEqual({ valid: true, errors: [], manifest: expect.objectContaining({ id: "widgets" }) });
  });
});

describe("validateStructural - load failures", () => {
  it("fails cleanly when module.js never calls osRegisterModule", async () => {
    const modulePath = await writeModule("widgets", `const x = 1;`);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/did not call osRegisterModule/);
  });

  it("fails cleanly on a syntax error instead of throwing", async () => {
    const modulePath = await writeModule("widgets", `this is not valid js {{{`);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/Failed to load module\.js/);
  });

  it("fails cleanly when the file doesn't exist", async () => {
    const result = await validateStructural(path.join(modulesDir, "missing", "module.js"), { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/Failed to load module\.js/);
  });
});

describe("validateStructural - ajv schema", () => {
  it("fails when a required top-level field is missing", async () => {
    const modulePath = await writeModule(
      "widgets",
      `osRegisterModule({ id: "widgets", entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } }, views: [{ id:"all", label:"All", kind:"list", type:"widget" }] });`,
    );
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("name"))).toBe(true);
  });

  it("fails when an attr has an unknown type", async () => {
    const source = VALID_SOURCE.replace('type: "text"', 'type: "wat"');
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
  });
});

describe("validateStructural - duplicate type ids", () => {
  it("fails when an entity type id collides with an existing module", async () => {
    await writeModule(
      "gadgets",
      `osRegisterModule({ id: "gadgets", name: "Gadgets", icon: "Zap", color: "red",
        entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } },
        views: [{ id: "all", label: "All", kind: "list", type: "widget" }] });`,
    );
    const modulePath = await writeModule("widgets", VALID_SOURCE);

    const result = await validateStructural(modulePath, { modulesDir });

    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes('"widget"') && e.includes("gadgets"))).toBe(true);
  });

  it("does not flag its own directory as a collision", async () => {
    const modulePath = await writeModule("widgets", VALID_SOURCE);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(true);
  });

  it("skips sibling directories it can't introspect instead of failing", async () => {
    await fs.mkdir(path.join(modulesDir, "broken"), { recursive: true });
    await fs.writeFile(path.join(modulesDir, "broken", "module.js"), "not valid js {{{", "utf8");
    const modulePath = await writeModule("widgets", VALID_SOURCE);

    const result = await validateStructural(modulePath, { modulesDir });

    expect(result.valid).toBe(true);
  });
});

describe("validateStructural - dangling view refs", () => {
  it("fails when a view's type doesn't match any entityTypes key", async () => {
    const source = VALID_SOURCE.replace('type: "widget"', 'type: "nonexistent"');
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("nonexistent"))).toBe(true);
  });

  it("fails when a metric-kind view's metric id isn't declared", async () => {
    const source = `osRegisterModule({
      id: "widgets", name: "Widgets", icon: "Zap", color: "red",
      entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } },
      views: [{ id: "chart", label: "Chart", kind: "metric", metric: "missing_metric" }],
      metrics: [{ id: "real_metric", label: "Real" }],
    });`;
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("missing_metric"))).toBe(true);
  });

  it("passes a metric-kind view whose metric id is declared", async () => {
    const source = `osRegisterModule({
      id: "widgets", name: "Widgets", icon: "Zap", color: "red",
      entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } },
      views: [{ id: "chart", label: "Chart", kind: "metric", metric: "real_metric" }],
      metrics: [{ id: "real_metric", label: "Real" }],
    });`;
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(true);
  });
});

// T1 (issue #133): the known-kinds set is now derived from rendererKinds.js
// (frontend/src/core/), not a hardcoded local list - `graph` passes now that
// a GenericGraph renderer exists, and a made-up kind still fails.
describe("validateStructural - kind list derived from rendererKinds.js", () => {
  it("passes a view declaring kind:'graph'", async () => {
    const source = `osRegisterModule({
      id: "widgets", name: "Widgets", icon: "Zap", color: "red",
      entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } },
      views: [{ id: "web", label: "Web", kind: "graph", type: "widget" }],
    });`;
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(true);
  });

  it("still fails a bogus, never-registered kind", async () => {
    const source = `osRegisterModule({
      id: "widgets", name: "Widgets", icon: "Zap", color: "red",
      entityTypes: { widget: { label: "W", plural: "Ws", icon: "Zap", attrs: {} } },
      views: [{ id: "web", label: "Web", kind: "hologram", type: "widget" }],
    });`;
    const modulePath = await writeModule("widgets", source);
    const result = await validateStructural(modulePath, { modulesDir });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes('"hologram"'))).toBe(true);
  });
});

// Regression (issue #133): modules/learning declares a real kind:"graph"
// view (docs/SELF-EXTENSION.md §4's noted finding) - it must now validate
// against the real modules/ tree, not just an isolated fixture dir.
describe("validateStructural - modules/learning regression", () => {
  it("passes the real modules/learning/module.js now that GenericGraph exists", async () => {
    const modulePath = path.join(REAL_MODULES_DIR, "learning", "module.js");
    const result = await validateStructural(modulePath, { modulesDir: REAL_MODULES_DIR });
    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });
});
