// Telegram voice-note intake (issue #143). The Worker holds no provider
// tokens and never transcribes or runs an agent itself - it downloads the
// voice note via the Telegram file API, carries the bytes as base64 in a
// `voice_turn` job payload (voice notes are small - ~100KB/min OGG), and lets
// the Mac drain (services/lifeos-drain) transcribe + run the agent turn + reply.
// Same "enqueue-only" discipline as `/ingest` and `/addmodule`.
import type { WorkerDb } from "@lifeos/db/client/worker";
import { enqueueJob } from "./jobs.js";

// Hard cap on a voice note we will carry inline as base64. Telegram voice
// notes are tiny; a large `message:audio` file is rejected with a friendly
// message rather than bloating a job payload (and the DB row).
export const MAX_VOICE_BYTES = 20 * 1024 * 1024;

// The subset of a Telegram voice/audio object this flow needs.
export interface TelegramVoiceLike {
  file_id: string;
  file_size?: number;
  mime_type?: string;
}

export function voiceTooLargeMessage(): string {
  return "That audio is too large - send a voice note under 20 MB.";
}

// Encodes bytes to base64 without Buffer (Workers-safe: btoa over a binary
// string, chunked so a large note never blows the argument stack).
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

// Fetches a Telegram file's bytes given the `file_path` from getFile. Uses the
// bot token in the file-download URL (the documented Telegram file API path).
export async function downloadTelegramFile(token: string, filePath: string): Promise<Uint8Array> {
  const url = `https://api.telegram.org/file/bot${token}/${filePath}`;
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`telegram file download failed: ${res.status}`);
  }
  return new Uint8Array(await res.arrayBuffer());
}

// Enqueues a `voice_turn` job the Mac drain will claim. Payload shape matches
// `lifeos_drain::VoiceTurnPayload`.
export async function enqueueVoiceTurn(
  db: WorkerDb,
  workspaceId: string,
  chatId: string,
  audioB64: string,
  mime: string | undefined,
  fileName: string | undefined,
): Promise<void> {
  await enqueueJob(db, workspaceId, "voice_turn", {
    chat_id: chatId,
    audio_b64: audioB64,
    mime: mime ?? null,
    file_name: fileName ?? null,
  });
}
