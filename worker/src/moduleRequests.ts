// `/addmodule` (issue #67, README.md's "Mac offline (queued)" flow): writes a
// `module_requests` row and replies "queued" - the bot never writes code or
// files itself (it's a Cloudflare Worker; there is no filesystem to write
// to). A LaunchAgent poller on the Mac (services/lifeos-drain -
// `claim_next_module_request`/`run_module_build`) claims `status='queued'`
// rows on wake and runs the real scaffold.js/validators build (or the T0-T5
// pipeline when `LIFEOS_BUILD_PIPELINE` is on), same as any other module
// install.
import { moduleRequests } from "@lifeos/db";
import type { WorkerDb } from "@lifeos/db/client/worker";
import { ulid } from "ulid";

export async function enqueueModuleRequest(
  db: WorkerDb,
  workspaceId: string,
  prompt: string,
  chatId?: string,
): Promise<string> {
  const id = `modreq_${ulid()}`;
  const now = Math.floor(Date.now() / 1000);
  await db.insert(moduleRequests).values({
    id,
    workspaceId,
    prompt,
    status: "queued",
    chatId: chatId ?? null,
    createdAt: now,
    updatedAt: now,
  });
  return id;
}
