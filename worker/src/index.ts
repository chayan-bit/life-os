// Cloudflare Worker entrypoint - issues #63-65 (docs/ARCHITECTURE.md §3.1).
// Routes Telegram's webhook POSTs into grammY via the native Workers
// adapter (`webhookCallback(bot, "cloudflare-mod")`); everything else is a
// bare liveness check for `wrangler deploy` smoke-testing.
import { webhookCallback } from "grammy";
import { createBot } from "./bot.js";
import { createDb, resolveWorkspaceId } from "./db.js";
import { buildDigest } from "./digest.js";

// Telegram sends this header on every webhook POST when the webhook was
// registered with a `secret_token` (see `setWebhook` note on `Env.
// TELEGRAM_WEBHOOK_SECRET` below) - grammY's `webhookCallback` also checks it
// internally when given `secretToken`, but `verifyTelegramWebhookSecret`
// below checks it BEFORE that, so a forged/wrong-secret request never reaches
// `bot.init()` (which would otherwise call Telegram's getMe) or the DB.
const TELEGRAM_SECRET_HEADER = "X-Telegram-Bot-Api-Secret-Token";

export interface Env {
  BOT_TOKEN: string;
  // DB + Haiku bindings (issues #64/#65).
  TURSO_URL: string;
  TURSO_TOKEN: string;
  // OPTIONAL - the bot's own light reasoning lane. Unset is fine: the bot
  // still does full DB CRUD/recall, and heavy or AI work is enqueued to the
  // Mac harness (jobs), which runs keyless through local agent CLIs
  // (services/lifeos-agents). Cloudflare can't exec CLIs, so this is the
  // only key left in the system, and it is opt-in.
  ANTHROPIC_API_KEY?: string;
  WORKSPACE_ID?: string;
  // issue #71 - where the scheduled digest is sent; unset = no digest
  // (manual-setup-gated, same as every other deploy-time value in this
  // file, see docs/MANUAL-SETUP.md). Get this by messaging the bot once and
  // reading the chat id off the update, or from @userinfobot.
  DIGEST_CHAT_ID?: string;
  // REQUIRED for `/telegram` (security audit finding 5): without it, anyone
  // who learns the deployed Worker URL can POST forged Telegram updates -
  // including the inline Approve callback, defeating the human-gating hard
  // rule (docs/ARCHITECTURE.md). Optional in the TYPE only, so environments
  // that never exercise `/telegram` (e.g. this file's own unit tests) still
  // compile; at runtime, unset means every `/telegram` POST is rejected
  // (fail closed - see `verifyTelegramWebhookSecret`), never "accept anything".
  //
  // Set with `wrangler secret put TELEGRAM_WEBHOOK_SECRET` (any random
  // string, e.g. `openssl rand -hex 32`), THEN pass the exact same value as
  // `secret_token` when registering the webhook with Telegram:
  //   curl "https://api.telegram.org/bot$BOT_TOKEN/setWebhook?url=https://<worker-subdomain>.workers.dev/telegram&secret_token=<the same value>"
  // (worker/README.md's existing `setWebhook` curl predates this - add
  // `&secret_token=...` to it when following that doc.)
  TELEGRAM_WEBHOOK_SECRET?: string;
  // Timezone the daily digest's "due today" window is computed in (finding
  // 41, correctness audit) - unset defaults to "Asia/Kolkata" per the user
  // (see commands.ts's `dayWindow`). Any IANA zone name works.
  LIFEOS_TZ?: string;
}

// Fail-closed secret check for `/telegram`, run before any DB/bot work: an
// unset secret rejects every call (never grammY's own default of "no token
// configured = accept anything"); a set-but-mismatched/missing header is
// likewise rejected. Only a header that matches is let through. Exported for
// direct unit testing without needing a live Telegram `getMe` call.
export function verifyTelegramWebhookSecret(request: Request, env: Env): boolean {
  if (!env.TELEGRAM_WEBHOOK_SECRET) {
    console.error("TELEGRAM_WEBHOOK_SECRET is unset - rejecting /telegram webhook (fail closed)");
    return false;
  }
  return request.headers.get(TELEGRAM_SECRET_HEADER) === env.TELEGRAM_WEBHOOK_SECRET;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === "/") {
      return new Response("ok", { status: 200 });
    }

    if (url.pathname === "/telegram" && request.method === "POST") {
      if (!verifyTelegramWebhookSecret(request, env)) {
        return new Response("unauthorized", { status: 401 });
      }

      const db = createDb(env);
      const workspaceId = resolveWorkspaceId(env);
      const bot = createBot({ token: env.BOT_TOKEN, db, workspaceId });
      // `secretToken` here is defense-in-depth (grammY re-checks the same
      // header internally) - `verifyTelegramWebhookSecret` above is what
      // actually keeps a forged/no-secret request from ever reaching
      // `bot.init()`/the DB.
      const handleUpdate = webhookCallback(bot, "cloudflare-mod", { secretToken: env.TELEGRAM_WEBHOOK_SECRET });
      return handleUpdate(request);
    }

    return new Response("not found", { status: 404 });
  },

  // Cloudflare Cron Trigger (wrangler.toml's `[triggers] crons`), issue #71.
  // No-ops when DIGEST_CHAT_ID isn't set - real send is verified live
  // post-deployment, same as `/telegram` (worker/test/index.test.ts).
  async scheduled(_event: ScheduledEvent, env: Env): Promise<void> {
    if (!env.DIGEST_CHAT_ID) return;

    const db = createDb(env);
    const workspaceId = resolveWorkspaceId(env);
    const digest = await buildDigest(db, workspaceId, Math.floor(Date.now() / 1000), env.LIFEOS_TZ);
    const bot = createBot({ token: env.BOT_TOKEN, db, workspaceId });
    await bot.api.sendMessage(Number(env.DIGEST_CHAT_ID), digest);
  },
};
