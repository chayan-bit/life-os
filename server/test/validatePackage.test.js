// Marketplace package validator tests (issue #147). Locks two things the Rust
// install route depends on: (1) validatePackage() enforces the real T0 gates
// (structural schema + view-ref resolution), and (2) the CLI wrapper emits the
// result as its LAST stdout line so the shelling side can parse it.
import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { validatePackage } from "../validators/validatePackage.js";

const execFile = promisify(execFileCb);
const ENTRY = path.resolve(import.meta.dirname, "..", "validators", "validatePackage.js");

const VALID_MANIFEST = {
  id: "reading",
  name: "Reading",
  icon: "Book",
  color: "var(--neo-mint)",
  version: "1.0.0",
  entityTypes: {
    book: { label: "Book", plural: "Books", icon: "Book", attrs: { title: { type: "text", required: true } } },
  },
  views: [{ id: "all", label: "All", kind: "list", type: "book" }],
};

let tmpDir;
beforeEach(async () => {
  tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-validate-pkg-"));
});
afterEach(async () => {
  await fs.rm(tmpDir, { recursive: true, force: true });
});

describe("validatePackage", () => {
  it("passes a well-formed T0 module manifest", async () => {
    const result = await validatePackage(VALID_MANIFEST);
    expect(result).toEqual({ valid: true, errors: [], tier: "T0" });
  });

  it("rejects a manifest missing required top-level fields", async () => {
    const result = await validatePackage({ id: "reading", version: "1.0.0" });
    expect(result.valid).toBe(false);
    expect(result.errors.length).toBeGreaterThan(0);
  });

  it("rejects a manifest whose view references an undeclared entity type", async () => {
    const bad = { ...VALID_MANIFEST, views: [{ id: "all", label: "All", kind: "list", type: "ghost" }] };
    const result = await validatePackage(bad);
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("ghost"))).toBe(true);
  });

  it("rejects a non-object / empty manifest without throwing", async () => {
    const result = await validatePackage({});
    expect(result.valid).toBe(false);
    expect(result.errors.length).toBeGreaterThan(0);
  });
});

describe("validatePackage CLI last-line contract", () => {
  async function runCli(manifest) {
    const file = path.join(tmpDir, "manifest.json");
    await fs.writeFile(file, JSON.stringify(manifest), "utf8");
    try {
      const { stdout } = await execFile("node", [ENTRY, file]);
      return stdout;
    } catch (error) {
      // A rejected manifest exits non-zero; its stdout still carries the JSON.
      return error.stdout ?? "";
    }
  }

  it("prints a valid result as the last stdout line for a good manifest", async () => {
    const stdout = await runCli(VALID_MANIFEST);
    const lastLine = stdout.trim().split("\n").pop();
    expect(JSON.parse(lastLine)).toEqual({ valid: true, errors: [], tier: "T0" });
  });

  it("prints an invalid result (non-zero exit) as the last stdout line for a bad manifest", async () => {
    const stdout = await runCli({ id: "reading", version: "1.0.0" });
    const lastLine = stdout.trim().split("\n").pop();
    const parsed = JSON.parse(lastLine);
    expect(parsed.valid).toBe(false);
    expect(parsed.errors.length).toBeGreaterThan(0);
  });
});
