import { beforeEach, describe, expect, it } from "vitest";
import type { LocalDb } from "@lifeos/db/client/local";
import { events, jobs } from "@lifeos/db";
import { eq } from "@lifeos/db/query";
import { approveEntity, denyEntity, listPendingApprovals } from "../src/approvals.js";
import { createEntity, getEntityById } from "../src/entities.js";
import { createTestDb } from "./testDb.js";

let db: LocalDb;
const WS = "ws_test";

beforeEach(async () => {
  db = await createTestDb();
});

async function seedDraft(workspaceId = WS) {
  return createEntity(db, workspaceId, {
    module: "bot",
    type: "draft",
    status: "pending_approval",
    attrs: { text: "announce the launch" },
  });
}

describe("listPendingApprovals", () => {
  it("returns only pending_approval entities for the workspace", async () => {
    await seedDraft();
    await createEntity(db, WS, { module: "tasks", type: "task", title: "unrelated", status: "open" });
    await seedDraft("ws_other");

    const rows = await listPendingApprovals(db, WS);

    expect(rows).toHaveLength(1);
    expect(rows[0].status).toBe("pending_approval");
  });
});

describe("approveEntity", () => {
  it("transitions to approved, records an event, and enqueues an execute_approval job", async () => {
    const draft = await seedDraft();

    const result = await approveEntity(db, WS, draft.id);

    expect(result.outcome).toBe("approved");
    const updated = await getEntityById(db, WS, draft.id);
    expect(updated?.status).toBe("approved");

    const recordedEvents = await db.select().from(events).where(eq(events.entityId, draft.id));
    expect(recordedEvents).toHaveLength(1);
    expect(recordedEvents[0].type).toBe("draft.approved");

    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(1);
    expect(queuedJobs[0].kind).toBe("execute_approval");
    expect(JSON.parse(queuedJobs[0].payload)).toEqual({ entity_id: draft.id, entity_type: "draft" });
  });

  it("is a no-op the second time (already_resolved), never double-enqueuing", async () => {
    const draft = await seedDraft();
    await approveEntity(db, WS, draft.id);

    const second = await approveEntity(db, WS, draft.id);

    expect(second.outcome).toBe("already_resolved");
    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(1);
  });

  it("reports not_found for an unknown id", async () => {
    const result = await approveEntity(db, WS, "ent_does_not_exist");
    expect(result.outcome).toBe("not_found");
  });

  it("cannot approve another workspace's draft", async () => {
    const draft = await seedDraft("ws_other");

    const result = await approveEntity(db, WS, draft.id);

    expect(result.outcome).toBe("not_found");
    const stillPending = await getEntityById(db, "ws_other", draft.id);
    expect(stillPending?.status).toBe("pending_approval");
  });
});

describe("approveEntity - typed confirm (issue #142)", () => {
  // A T5 gate is a server/build/gate.js build gate, so its real status is
  // "awaiting_approval" (finding 13) - seeding "pending_approval" here would
  // still happen to work and mask that the Worker used to only recognize
  // that one status, excluding every real T5 gate from this whole flow.
  async function seedTypedGate() {
    return createEntity(db, WS, {
      module: "pipelines",
      type: "pending_approval",
      title: "T5 crate",
      status: "awaiting_approval",
      attrs: { requires_typed_confirm: true, node: "t5" },
    });
  }

  it("refuses a bare tap (no typed phrase) on a typed-confirm gate", async () => {
    const gate = await seedTypedGate();

    const result = await approveEntity(db, WS, gate.id);

    expect(result.outcome).toBe("requires_typed_confirm");
    if (result.outcome === "requires_typed_confirm") expect(result.phrase).toBe("t5");
    const still = await getEntityById(db, WS, gate.id);
    expect(still?.status).toBe("awaiting_approval");
    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(0);
  });

  it("refuses a wrong typed phrase", async () => {
    const gate = await seedTypedGate();
    const result = await approveEntity(db, WS, gate.id, "nope");
    expect(result.outcome).toBe("requires_typed_confirm");
  });

  it("approves when the exact phrase is typed, enqueuing execute_approval", async () => {
    const gate = await seedTypedGate();

    const result = await approveEntity(db, WS, gate.id, "t5");

    expect(result.outcome).toBe("approved");
    const updated = await getEntityById(db, WS, gate.id);
    expect(updated?.status).toBe("approved");
    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(1);
    expect(queuedJobs[0].kind).toBe("execute_approval");
  });
});

describe("denyEntity", () => {
  it("transitions to rejected (matching services/lifeos-api), records a *.rejected event, and enqueues nothing", async () => {
    const draft = await seedDraft();

    const result = await denyEntity(db, WS, draft.id);

    expect(result.outcome).toBe("denied");
    const updated = await getEntityById(db, WS, draft.id);
    // Not "denied" - services/lifeos-api/src/routes/approval.rs's deny CAS-
    // transitions to "rejected"; the Worker previously wrote a second,
    // API-unrecognized terminal status for the same entity (finding 13).
    expect(updated?.status).toBe("rejected");

    const recordedEvents = await db.select().from(events).where(eq(events.entityId, draft.id));
    expect(recordedEvents).toHaveLength(1);
    expect(recordedEvents[0].type).toBe("draft.rejected");

    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(0);
  });
});

// finding 13: server/build/gate.js's real T3+ build gates carry status
// "awaiting_approval", not "pending_approval" - the typed-confirm tests above
// used "pending_approval" for their gate fixture, which happened to already
// work and masked the fact that a REAL gate (this status) was silently
// excluded from every pending-approval surface. These seed the real status.
describe("awaiting_approval gates (server/build/gate.js's real status)", () => {
  async function seedBuildGate(workspaceId = WS) {
    return createEntity(db, workspaceId, {
      module: "pipelines",
      type: "pending_approval",
      title: "T3 crate",
      status: "awaiting_approval",
    });
  }

  it("listPendingApprovals includes awaiting_approval gates alongside pending_approval drafts", async () => {
    const gate = await seedBuildGate();
    const draft = await seedDraft();

    const rows = await listPendingApprovals(db, WS);

    expect(rows.map((r) => r.id).sort()).toEqual([draft.id, gate.id].sort());
  });

  it("approveEntity approves an awaiting_approval gate, transitioning it and enqueuing execute_approval", async () => {
    const gate = await seedBuildGate();

    const result = await approveEntity(db, WS, gate.id);

    expect(result.outcome).toBe("approved");
    const updated = await getEntityById(db, WS, gate.id);
    expect(updated?.status).toBe("approved");
    const queuedJobs = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(queuedJobs).toHaveLength(1);
    expect(queuedJobs[0].kind).toBe("execute_approval");
  });

  it("denyEntity rejects an awaiting_approval gate", async () => {
    const gate = await seedBuildGate();

    const result = await denyEntity(db, WS, gate.id);

    expect(result.outcome).toBe("denied");
    const updated = await getEntityById(db, WS, gate.id);
    expect(updated?.status).toBe("rejected");
  });
});
