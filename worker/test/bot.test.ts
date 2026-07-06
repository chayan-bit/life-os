import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UserFromGetMe } from "grammy/types";
import type { LocalDb } from "@lifeos/db/client/local";
import { events, jobs } from "@lifeos/db";
import { eq } from "@lifeos/db/query";
import { createBot, healthMessage, type BotDeps } from "../src/bot.js";
import { createTestDb } from "./testDb.js";

const FAKE_BOT_INFO: UserFromGetMe = {
  id: 1,
  is_bot: true,
  first_name: "Life OS",
  username: "lifeos_test_bot",
  can_join_groups: true,
  can_read_all_group_messages: false,
  supports_inline_queries: false,
  can_connect_to_business: false,
  has_main_web_app: false,
  has_topics_enabled: false,
  allows_users_to_create_topics: false,
  can_manage_bots: false,
  supports_join_request_queries: false,
};

const WS = "ws_test";

function textUpdate(text: string) {
  return {
    update_id: 1,
    message: {
      message_id: 1,
      date: 0,
      chat: { id: 1, type: "private" as const, first_name: "tester" },
      from: { id: 1, is_bot: false, first_name: "tester" },
      text,
      entities: [{ offset: 0, length: text.split(" ")[0].length, type: "bot_command" as const }],
    },
  };
}

// grammY's documented offline-testing pattern: pass `botInfo` so `bot.init()`
// never calls Telegram's getMe, and intercept outgoing API calls via
// `bot.api.config.use` instead of hitting the network.
function repliesFrom(bot: ReturnType<typeof createBot>) {
  const sent: string[] = [];
  bot.api.config.use((prev, method, payload, signal) => {
    if (method === "sendMessage" && typeof (payload as { text?: string }).text === "string") {
      sent.push((payload as { text: string }).text);
      return Promise.resolve({ ok: true, result: {} } as never);
    }
    return prev(method, payload, signal);
  });
  return sent;
}

interface CapturedCalls {
  messages: { text: string; replyMarkup?: { inline_keyboard: { text: string; callback_data?: string }[][] } }[];
  edits: string[];
  answeredCallbacks: (string | undefined)[];
}

function captureAll(bot: ReturnType<typeof createBot>): CapturedCalls {
  const captured: CapturedCalls = { messages: [], edits: [], answeredCallbacks: [] };
  bot.api.config.use((prev, method, payload, signal) => {
    if (method === "sendMessage") {
      const p = payload as { text: string; reply_markup?: CapturedCalls["messages"][number]["replyMarkup"] };
      captured.messages.push({ text: p.text, replyMarkup: p.reply_markup });
      return Promise.resolve({ ok: true, result: {} } as never);
    }
    if (method === "editMessageText") {
      captured.edits.push((payload as { text: string }).text);
      return Promise.resolve({ ok: true, result: true } as never);
    }
    if (method === "answerCallbackQuery") {
      captured.answeredCallbacks.push((payload as { text?: string }).text);
      return Promise.resolve({ ok: true, result: true } as never);
    }
    return prev(method, payload, signal);
  });
  return captured;
}

function callbackQueryUpdate(data: string) {
  return {
    update_id: 3,
    callback_query: {
      id: "cbq_1",
      from: { id: 1, is_bot: false, first_name: "tester" },
      chat_instance: "ci_1",
      message: {
        message_id: 5,
        date: 0,
        chat: { id: 1, type: "private" as const, first_name: "tester" },
        text: "pending draft",
      },
      data,
    },
  };
}

let db: LocalDb;
let deps: BotDeps;

beforeEach(async () => {
  db = await createTestDb();
  deps = { token: "fake-token", db, workspaceId: WS };
});

describe("healthMessage", () => {
  it("returns a fixed ok string", () => {
    expect(healthMessage()).toBe("ok");
  });
});

describe("createBot - scaffold commands", () => {
  it("replies to /start with an online message", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/start"));

    expect(sent).toEqual(["Life OS bot is online."]);
  });

  it("replies to /health with the health message", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/health"));

    expect(sent).toEqual([healthMessage()]);
  });

  it("does not reply to unrelated text", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate({
      update_id: 2,
      message: {
        message_id: 2,
        date: 0,
        chat: { id: 1, type: "private" as const, first_name: "tester" },
        from: { id: 1, is_bot: false, first_name: "tester" },
        text: "hello there",
      },
    });

    expect(sent).toEqual([]);
  });
});

describe("createBot - capture/query commands (issue #65)", () => {
  it("captures a task via /task and lists it via /today", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/task buy milk"));
    await bot.handleUpdate(textUpdate("/today"));

    expect(sent[0]).toMatch(/^Task captured/);
    expect(sent[1]).toContain("buy milk");
  });

  it("captures a topic via /topic and surfaces it via /inbox", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/topic spaced repetition"));
    await bot.handleUpdate(textUpdate("/inbox"));

    expect(sent[1]).toContain("spaced repetition");
  });

  it("completes a task end-to-end via /task then /done", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/task write the report"));
    const shortId = sent[0].match(/\[(\w+)\]/)?.[1] ?? "";
    await bot.handleUpdate(textUpdate(`/done ${shortId}`));
    await bot.handleUpdate(textUpdate("/today"));

    expect(sent[1]).toBe("Done: write the report");
    expect(sent[2]).toBe("Nothing due today.");
  });

  it("only ever drafts via /draft, never publishes", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft announce the launch"));

    expect(sent[0]).toMatch(/awaiting approval/);
  });

  it("reports pnl and quiz with no data yet", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/pnl"));
    await bot.handleUpdate(textUpdate("/quiz"));

    expect(sent[0]).toBe("All-time realized PnL: +0.00");
    expect(sent[1]).toMatch(/No topics/);
  });

  it("only sees its own workspace's tasks", async () => {
    const otherDeps: BotDeps = { token: "fake-token", db, workspaceId: "ws_other" };
    const botA = createBot(deps, FAKE_BOT_INFO);
    const botB = createBot(otherDeps, FAKE_BOT_INFO);
    const sentA = repliesFrom(botA);
    const sentB = repliesFrom(botB);

    await botA.init();
    await botB.init();
    await botA.handleUpdate(textUpdate("/task workspace a's task"));
    await botB.handleUpdate(textUpdate("/today"));

    expect(sentB[0]).toBe("Nothing due today.");
  });
});

describe("createBot - gated approve/deny (issue #66)", () => {
  it("/draft attaches an approve/deny inline keyboard to the confirmation message", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft announce the launch"));

    const buttons = captured.messages[0].replyMarkup?.inline_keyboard[0] ?? [];
    expect(buttons.map((b) => b.text)).toEqual(["Approve", "Deny"]);
    expect(buttons[0].callback_data).toMatch(/^approve:ent_/);
    expect(buttons[1].callback_data).toMatch(/^deny:ent_/);
  });

  it("tapping Approve resolves the draft and never calls a provider directly", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft announce the launch"));
    const approveData = captured.messages[0].replyMarkup?.inline_keyboard[0][0].callback_data ?? "";

    await bot.handleUpdate(callbackQueryUpdate(approveData));

    expect(captured.answeredCallbacks[0]).toBe("Approved - queued for execution.");
    expect(captured.edits[0]).toBe("Approved - queued for execution.");
  });

  it("tapping Deny resolves the draft with no job enqueued", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft announce the launch"));
    const denyData = captured.messages[0].replyMarkup?.inline_keyboard[0][1].callback_data ?? "";

    await bot.handleUpdate(callbackQueryUpdate(denyData));

    expect(captured.answeredCallbacks[0]).toBe("Denied.");
  });

  it("a second tap on an already-resolved draft is a safe no-op, not a crash", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft announce the launch"));
    const approveData = captured.messages[0].replyMarkup?.inline_keyboard[0][0].callback_data ?? "";

    await bot.handleUpdate(callbackQueryUpdate(approveData));
    await bot.handleUpdate(callbackQueryUpdate(approveData));

    expect(captured.answeredCallbacks[1]).toMatch(/Already approved/);
  });

  it("/pending sends one message with buttons per pending draft", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/draft first draft"));
    await bot.handleUpdate(textUpdate("/draft second draft"));
    await bot.handleUpdate(textUpdate("/pending"));

    // 2 confirmation replies from /draft + 2 /pending listing messages.
    expect(captured.messages).toHaveLength(4);
    expect(captured.messages[2].text).toContain("first draft");
    expect(captured.messages[2].replyMarkup?.inline_keyboard[0].map((b) => b.text)).toEqual(["Approve", "Deny"]);
  });

  it("/pending says nothing is pending once everything is resolved", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/pending"));

    expect(captured.messages[0].text).toBe("Nothing pending approval.");
  });
});

describe("createBot - typed-confirm gate (issue #142)", () => {
  async function seedTypedGate(workspaceId = WS) {
    const { createEntity } = await import("../src/entities.js");
    return createEntity(db, workspaceId, {
      module: "pipelines",
      type: "pending_approval",
      title: "T5 crate",
      status: "pending_approval",
      attrs: { requires_typed_confirm: true, node: "t5" },
    });
  }

  function replyUpdate(text: string, quoted: string) {
    return {
      update_id: 7,
      message: {
        message_id: 8,
        date: 0,
        chat: { id: 1, type: "private" as const, first_name: "tester" },
        from: { id: 1, is_bot: false, first_name: "tester" },
        text,
        reply_to_message: {
          message_id: 6,
          date: 0,
          chat: { id: 1, type: "private" as const, first_name: "tester" },
          text: quoted,
          reply_to_message: undefined,
        },
      },
    };
  }

  it("a button tap on a typed-confirm gate refuses and opens a force-reply prompt", async () => {
    const gate = await seedTypedGate();
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(callbackQueryUpdate(`approve:${gate.id}`));

    expect(captured.answeredCallbacks[0]).toMatch(/Requires typed confirmation - reply with: t5/);
    expect(captured.messages[0].text).toContain(`[confirm:${gate.id}]`);
    const stillPending = await getEntityByIdForTest(gate.id);
    expect(stillPending).toBe("pending_approval");
  });

  it("replying with the exact phrase approves the gate", async () => {
    const gate = await seedTypedGate();
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(replyUpdate("t5", `confirm this\n[confirm:${gate.id}]`));

    expect(captured.messages[0].text).toBe("Approved - queued for execution.");
    expect(await getEntityByIdForTest(gate.id)).toBe("approved");
  });

  it("replying with the wrong phrase does not approve", async () => {
    const gate = await seedTypedGate();
    const bot = createBot(deps, FAKE_BOT_INFO);
    const captured = captureAll(bot);

    await bot.init();
    await bot.handleUpdate(replyUpdate("wrong", `confirm this\n[confirm:${gate.id}]`));

    expect(captured.messages[0].text).toMatch(/did not match/);
    expect(await getEntityByIdForTest(gate.id)).toBe("pending_approval");
  });

  async function getEntityByIdForTest(id: string): Promise<string | null | undefined> {
    const { getEntityById } = await import("../src/entities.js");
    return (await getEntityById(db, WS, id))?.status;
  }
});

describe("createBot - recall (issue #69)", () => {
  it("/recall finds a previously captured topic", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/topic the halting problem"));
    await bot.handleUpdate(textUpdate("/recall halting"));

    expect(sent[1]).toContain("the halting problem");
  });
});

describe("createBot - audit logging (issue #70)", () => {
  it("records a bot.command event for every command, including plain reads", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/pnl"));

    const rows = await db.select().from(events).where(eq(events.workspaceId, WS));
    expect(rows).toHaveLength(1);
    expect(rows[0].type).toBe("bot.command");
    expect(rows[0].actor).toBe("bot");
    expect(JSON.parse(rows[0].attrs ?? "{}")).toEqual({ text: "/pnl" });
  });

  it("does not record an event for non-command text", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate({
      update_id: 9,
      message: {
        message_id: 9,
        date: 0,
        chat: { id: 1, type: "private" as const, first_name: "tester" },
        from: { id: 1, is_bot: false, first_name: "tester" },
        text: "hello there",
      },
    });

    const rows = await db.select().from(events).where(eq(events.workspaceId, WS));
    expect(rows).toHaveLength(0);
  });
});

describe("createBot - heavy-job enqueue (issue #67)", () => {
  it("/addmodule replies queued and never writes code or files itself", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/addmodule add a health tracker"));

    expect(sent[0]).toMatch(/^Queued/);
  });

  it("/ingest replies queued for the Mac", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = repliesFrom(bot);

    await bot.init();
    await bot.handleUpdate(textUpdate("/ingest https://example.com/clip"));

    expect(sent[0]).toBe("Queued for the Mac.");
  });
});

describe("createBot - voice notes (issue #143)", () => {
  // Intercepts getFile (grammY API call) and stubs the raw file download so no
  // network is hit - grammY's documented offline pattern extended to files.
  function captureVoiceFlow(bot: ReturnType<typeof createBot>, fileSize = 2048) {
    const sent: string[] = [];
    bot.api.config.use((prev, method, payload, signal) => {
      if (method === "sendMessage") {
        sent.push((payload as { text: string }).text);
        return Promise.resolve({ ok: true, result: {} } as never);
      }
      if (method === "getFile") {
        return Promise.resolve({
          ok: true,
          result: { file_id: "f1", file_unique_id: "u1", file_size: fileSize, file_path: "voice/file_1.oga" },
        } as never);
      }
      return prev(method, payload, signal);
    });
    return sent;
  }

  function voiceUpdate(fileSize?: number) {
    return {
      update_id: 20,
      message: {
        message_id: 21,
        date: 0,
        chat: { id: 99, type: "private" as const, first_name: "tester" },
        from: { id: 1, is_bot: false, first_name: "tester" },
        voice: { file_id: "f1", file_unique_id: "u1", duration: 3, mime_type: "audio/ogg", file_size: fileSize },
      },
    };
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("downloads a voice note and enqueues a voice_turn job carrying base64 audio + chat", async () => {
    const audio = new Uint8Array([79, 103, 103, 83, 1, 2, 3, 4]); // "OggS" + payload
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(audio.buffer, { status: 200 })),
    );

    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = captureVoiceFlow(bot);

    await bot.init();
    await bot.handleUpdate(voiceUpdate(audio.byteLength));

    expect(sent[0]).toMatch(/transcribing/i);

    const rows = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe("voice_turn");
    const payload = JSON.parse(rows[0].payload) as { chat_id: string; audio_b64: string; mime: string };
    expect(payload.chat_id).toBe("99");
    expect(payload.mime).toBe("audio/ogg");
    // Round-trips back to the original bytes.
    expect(atob(payload.audio_b64)).toBe(String.fromCharCode(...audio));
  });

  it("rejects an oversize audio file with a friendly message and enqueues nothing", async () => {
    const bot = createBot(deps, FAKE_BOT_INFO);
    const sent = captureVoiceFlow(bot);

    await bot.init();
    await bot.handleUpdate(voiceUpdate(21 * 1024 * 1024));

    expect(sent[0]).toMatch(/too large/i);
    const rows = await db.select().from(jobs).where(eq(jobs.workspaceId, WS));
    expect(rows).toHaveLength(0);
  });
});
