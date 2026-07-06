import { beforeEach, describe, expect, it } from "vitest";
import type { LocalDb } from "@lifeos/db/client/local";
import { captureDraft, captureTask, captureTopic } from "../src/commands.js";
import { buildDigest } from "../src/digest.js";
import { createEntity } from "../src/entities.js";
import { createTestDb } from "./testDb.js";

let db: LocalDb;
const WS = "ws_test";
const NOW = 1_000_000;

beforeEach(async () => {
  db = await createTestDb();
});

describe("buildDigest", () => {
  it("rolls up due tasks, the inbox, PnL, and pending approvals", async () => {
    await captureTask(db, WS, "buy milk");
    await captureTopic(db, WS, "spaced repetition"); // lands in the inbox
    await captureDraft(db, WS, "announce the launch");

    const digest = await buildDigest(db, WS, NOW);

    expect(digest).toContain("buy milk");
    expect(digest).toContain("spaced repetition");
    expect(digest).toContain("announce the launch");
    expect(digest).toContain("All-time realized PnL: +0.00");
  });

  it("reports empty sections cleanly when there's nothing to show", async () => {
    const digest = await buildDigest(db, WS, NOW);

    expect(digest).toContain("Nothing due today.");
    expect(digest).toContain("Inbox is empty.");
    expect(digest).toContain("Nothing pending approval.");
  });

  it("only rolls up the given workspace", async () => {
    await captureTask(db, "ws_other", "someone else's task");

    const digest = await buildDigest(db, WS, NOW);

    expect(digest).not.toContain("someone else's task");
  });

  // finding 13: a real server/build/gate.js build gate carries status
  // "awaiting_approval", not "pending_approval" - it must show up in the
  // digest's pending-approval section same as a plain draft.
  it("includes awaiting_approval build gates in the pending approval section", async () => {
    await createEntity(db, WS, {
      module: "pipelines",
      type: "pending_approval",
      title: "T3 crate",
      status: "awaiting_approval",
    });

    const digest = await buildDigest(db, WS, NOW);

    expect(digest).toContain("T3 crate");
  });
});
