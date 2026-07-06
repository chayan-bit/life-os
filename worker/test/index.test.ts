import { describe, expect, it, vi } from "vitest";
import worker, { verifyTelegramWebhookSecret } from "../src/index.js";

// grammY's real `webhookCallback` calls `bot.init()` first, which hits
// Telegram's actual `getMe` over the network via the `node-fetch` package -
// a real `require()` inside grammY's own CJS build that Vitest's module
// graph never sees, so mocking "node-fetch" (or stubbing `globalThis.fetch`,
// bot.test.ts's pattern for the unrelated voice-note download path) can't
// reach it. Mocking `webhookCallback` itself is the seam that actually
// matters for finding 5: it proves index.ts reaches the "hand off to grammY"
// step (i.e. the secret check let a correctly-authenticated request through)
// without re-testing grammY's own network/update-handling internals, which
// are exercised for real in bot.test.ts and are out of scope here.
vi.mock("grammy", async (importOriginal) => {
  const actual = await importOriginal<typeof import("grammy")>();
  return { ...actual, webhookCallback: vi.fn(() => async () => new Response(null, { status: 200 })) };
});

const TEST_ENV = {
  BOT_TOKEN: "fake",
  TURSO_URL: "libsql://fake",
  TURSO_TOKEN: "fake",
  ANTHROPIC_API_KEY: "fake",
};

const SECRET_HEADER = "X-Telegram-Bot-Api-Secret-Token";

// Only the non-Telegram-facing routes are unit-testable without a network
// call to Telegram's getMe (webhookCallback initializes the bot lazily on
// first request) - the `/telegram` path is verified live post-deployment,
// same as every other manual-setup-gated integration in this repo.
describe("worker fetch handler", () => {
  it("returns 200 ok on the liveness route", async () => {
    const res = await worker.fetch(new Request("https://example.com/"), TEST_ENV);
    expect(res.status).toBe(200);
    expect(await res.text()).toBe("ok");
  });

  it("returns 404 on unknown routes", async () => {
    const res = await worker.fetch(new Request("https://example.com/nope"), TEST_ENV);
    expect(res.status).toBe(404);
  });

  it("returns 404 for a GET on the telegram webhook path", async () => {
    const res = await worker.fetch(new Request("https://example.com/telegram"), TEST_ENV);
    expect(res.status).toBe(404);
  });
});

// security audit finding 5: without a verified secret, anyone who learns the
// deployed Worker URL can POST forged Telegram updates (including the inline
// Approve callback), defeating human-gating. These never need a real
// Telegram `getMe` call: a wrong/missing secret is rejected before `bot.init`
// or the DB are ever touched.
describe("verifyTelegramWebhookSecret", () => {
  function requestWithHeader(header?: string): Request {
    const headers = header !== undefined ? { [SECRET_HEADER]: header } : undefined;
    return new Request("https://example.com/telegram", { method: "POST", headers, body: "{}" });
  }

  it("rejects when TELEGRAM_WEBHOOK_SECRET is unset (fail closed), even with a header", () => {
    expect(verifyTelegramWebhookSecret(requestWithHeader("anything"), TEST_ENV)).toBe(false);
    expect(verifyTelegramWebhookSecret(requestWithHeader(), TEST_ENV)).toBe(false);
  });

  it("rejects a missing secret header", () => {
    const env = { ...TEST_ENV, TELEGRAM_WEBHOOK_SECRET: "shh" };
    expect(verifyTelegramWebhookSecret(requestWithHeader(), env)).toBe(false);
  });

  it("rejects a wrong secret header", () => {
    const env = { ...TEST_ENV, TELEGRAM_WEBHOOK_SECRET: "shh" };
    expect(verifyTelegramWebhookSecret(requestWithHeader("nope"), env)).toBe(false);
  });

  it("accepts the correct secret header", () => {
    const env = { ...TEST_ENV, TELEGRAM_WEBHOOK_SECRET: "shh" };
    expect(verifyTelegramWebhookSecret(requestWithHeader("shh"), env)).toBe(true);
  });
});

describe("worker fetch handler - /telegram webhook secret (finding 5)", () => {
  it("rejects a POST with no TELEGRAM_WEBHOOK_SECRET configured, never touching the bot/DB", async () => {
    const res = await worker.fetch(
      new Request("https://example.com/telegram", { method: "POST", body: "{}" }),
      TEST_ENV,
    );
    expect(res.status).toBe(401);
  });

  it("rejects a POST with a wrong secret header", async () => {
    const env = { ...TEST_ENV, TELEGRAM_WEBHOOK_SECRET: "correct-secret" };
    const res = await worker.fetch(
      new Request("https://example.com/telegram", {
        method: "POST",
        headers: { [SECRET_HEADER]: "wrong-secret" },
        body: "{}",
      }),
      env,
    );
    expect(res.status).toBe(401);
  });

  it("accepts a POST with the correct secret header and hands it to grammY", async () => {
    const { webhookCallback } = await import("grammy");
    const env = { ...TEST_ENV, TELEGRAM_WEBHOOK_SECRET: "correct-secret" };

    const res = await worker.fetch(
      new Request("https://example.com/telegram", {
        method: "POST",
        headers: { [SECRET_HEADER]: "correct-secret" },
        body: JSON.stringify({ update_id: 1 }),
      }),
      env,
    );

    expect(res.status).toBe(200);
    // Defense-in-depth: grammY's own webhookCallback also gets the secret,
    // so it re-checks Telegram's header internally even if this file's own
    // pre-check (verifyTelegramWebhookSecret) is ever bypassed by a refactor.
    expect(vi.mocked(webhookCallback)).toHaveBeenCalledWith(expect.anything(), "cloudflare-mod", {
      secretToken: "correct-secret",
    });
  });
});

// issue #71: `scheduled` no-ops without DIGEST_CHAT_ID - unit-testable since
// that path never touches the DB or the network. The send-a-real-digest path
// needs a live TURSO_URL + Telegram, so it's verified post-deployment, same
// as `/telegram` above.
describe("worker scheduled handler", () => {
  it("does nothing when DIGEST_CHAT_ID is unset", async () => {
    await expect(worker.scheduled(undefined as never, TEST_ENV)).resolves.toBeUndefined();
  });
});
