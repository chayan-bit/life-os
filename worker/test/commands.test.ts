import { beforeEach, describe, expect, it } from "vitest";
import type { LocalDb } from "@lifeos/db/client/local";
import { events, jobs, moduleRequests } from "@lifeos/db";
import { eq } from "@lifeos/db/query";
import { captureDraft, captureTask, captureTopic, dayWindow, inbox, ingest, markDone, pnl, quiz, recall, requestModule, today } from "../src/commands.js";
import { createEntity, listEntities } from "../src/entities.js";
import { createTestDb } from "./testDb.js";

let db: LocalDb;
const WS = "ws_test";

beforeEach(async () => {
  db = await createTestDb();
});

describe("captureTask", () => {
  it("creates an open task and confirms it", async () => {
    const reply = await captureTask(db, WS, "buy milk");

    expect(reply).toMatch(/^Task captured \[\w+\]: buy milk$/);
    const rows = await listEntities(db, WS, { module: "tasks", type: "task" });
    expect(rows).toHaveLength(1);
    expect(rows[0].status).toBe("open");
  });

  it("rejects an empty task", async () => {
    expect(await captureTask(db, WS, "   ")).toMatch(/^Usage:/);
  });
});

describe("captureTopic", () => {
  it("creates an uncategorized topic (shows up in the inbox)", async () => {
    await captureTopic(db, WS, "quantum computing");

    expect(await inbox(db, WS)).toContain("quantum computing");
  });
});

describe("captureDraft", () => {
  it("creates a pending_approval draft, never publishing anything", async () => {
    const { reply, entity } = await captureDraft(db, WS, "post about the launch");

    expect(reply).toMatch(/awaiting approval/);
    expect(entity?.status).toBe("pending_approval");
    const rows = await listEntities(db, WS, { module: "bot", type: "draft" });
    expect(rows[0].status).toBe("pending_approval");
  });

  it("returns no entity for empty input", async () => {
    const { reply, entity } = await captureDraft(db, WS, "   ");
    expect(reply).toMatch(/^Usage:/);
    expect(entity).toBeNull();
  });
});

describe("markDone", () => {
  it("completes a task captured moments earlier, addressed by its short id", async () => {
    const captureReply = await captureTask(db, WS, "ship it");
    const shortId = captureReply.match(/\[(\w+)\]/)?.[1] ?? "";

    const doneReply = await markDone(db, WS, shortId);

    expect(doneReply).toBe("Done: ship it");
  });

  it("reports no match for an unknown id", async () => {
    expect(await markDone(db, WS, "nomatch")).toMatch(/No open task/);
  });
});

describe("today", () => {
  it("lists undated open tasks", async () => {
    await captureTask(db, WS, "no due date yet");

    expect(await today(db, WS, 1_000)).toContain("no due date yet");
  });

  it("says nothing due when there are no open tasks", async () => {
    expect(await today(db, WS, 1_000)).toBe("Nothing due today.");
  });

  // finding 41: 2026-07-05T20:00:00Z is 2026-07-06 01:30 IST - already
  // tomorrow's calendar day in Asia/Kolkata while still today in UTC. A task
  // due later that same IST day (2026-07-06T10:00:00Z, 3:30pm IST) must show
  // up under the IST window and must NOT under the old plain-UTC one.
  it("follows the IST calendar day, not the UTC one, by default", async () => {
    const now = Date.parse("2026-07-05T20:00:00Z") / 1000;
    const dueLaterTodayIst = Date.parse("2026-07-06T10:00:00Z") / 1000;
    await createEntity(db, WS, { module: "tasks", type: "task", title: "ist afternoon task", status: "open", attrs: { due: dueLaterTodayIst } });

    expect(await today(db, WS, now)).toContain("ist afternoon task");
    expect(await today(db, WS, now, "UTC")).not.toContain("ist afternoon task");
  });
});

describe("dayWindow", () => {
  it("defaults to Asia/Kolkata (IST, +05:30) - the IST day can differ from the UTC day", () => {
    // 2026-07-05T20:00:00Z = 2026-07-06T01:30:00 IST: already the 6th in
    // IST, still the 5th in UTC - exactly the boundary finding 41 covers.
    const now = Date.parse("2026-07-05T20:00:00Z") / 1000;

    const window = dayWindow(now);

    expect(window.startIso).toBe("2026-07-05T18:30:00.000Z"); // 2026-07-06T00:00:00 IST
    expect(window.endIso).toBe("2026-07-06T18:29:59.000Z"); // 2026-07-06T23:59:59 IST
    expect(window.startSecs).toBe(Date.parse("2026-07-05T18:30:00.000Z") / 1000);
    expect(window.endSecs).toBe(Date.parse("2026-07-06T18:29:59.000Z") / 1000);
  });

  it("accepts an explicit tz, e.g. plain UTC", () => {
    const now = Date.parse("2026-07-05T20:00:00Z") / 1000;

    const window = dayWindow(now, "UTC");

    expect(window.startIso).toBe("2026-07-05T00:00:00.000Z");
    expect(window.endIso).toBe("2026-07-05T23:59:59.000Z");
  });
});

describe("inbox", () => {
  it("is empty until something is captured without a status", async () => {
    expect(await inbox(db, WS)).toBe("Inbox is empty.");
    await captureTopic(db, WS, "first topic");
    expect(await inbox(db, WS)).toContain("first topic");
  });
});

describe("pnl", () => {
  it("reports the realized total from trade.closed events", async () => {
    await db.insert(events).values({
      id: "evt_1",
      workspaceId: WS,
      ts: 1,
      type: "trade.closed",
      attrs: JSON.stringify({ pnl: 42.5 }),
    });

    expect(await pnl(db, WS)).toBe("All-time realized PnL: +42.50");
  });
});

describe("quiz", () => {
  it("prompts about a captured topic", async () => {
    await captureTopic(db, WS, "the halting problem");

    expect(await quiz(db, WS)).toContain("the halting problem");
  });

  it("tells you to capture a topic first when there are none", async () => {
    expect(await quiz(db, WS)).toMatch(/No topics/);
  });
});

describe("requestModule", () => {
  it("queues a module_requests row and never touches code/files", async () => {
    const reply = await requestModule(db, WS, "add a health tracker module");

    expect(reply).toMatch(/^Queued/);
    const rows = await db.select().from(moduleRequests).where(eq(moduleRequests.workspaceId, WS));
    expect(rows).toHaveLength(1);
    expect(rows[0].status).toBe("queued");
    expect(rows[0].prompt).toBe("add a health tracker module");
  });

  it("rejects an empty request", async () => {
    expect(await requestModule(db, WS, "  ")).toMatch(/^Usage:/);
  });
});

describe("recall", () => {
  it("cites the matching entity by short id", async () => {
    const captureReply = await captureTopic(db, WS, "the halting problem");
    const shortId = captureReply.match(/\[(\w+)\]/)?.[1] ?? "";

    const reply = await recall(db, WS, "halting");

    expect(reply).toContain(`[${shortId}]`);
    expect(reply).toContain("the halting problem");
  });

  it("reports no match", async () => {
    expect(await recall(db, WS, "nothing captured yet")).toMatch(/^Nothing found/);
  });

  it("rejects an empty query", async () => {
    expect(await recall(db, WS, "  ")).toMatch(/^Usage:/);
  });
});

describe("ingest", () => {
  it("queues a jobs row for the Mac to drain", async () => {
    const reply = await ingest(db, WS, "https://example.com/some-video");

    expect(reply).toBe("Queued for the Mac.");
    const rows = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe("ingest");
    expect(JSON.parse(rows[0].payload)).toEqual({ text: "https://example.com/some-video" });
  });

  it("rejects an empty ingest request", async () => {
    expect(await ingest(db, WS, "")).toMatch(/^Usage:/);
  });
});
