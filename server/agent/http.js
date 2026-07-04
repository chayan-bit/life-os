// Default HTTP layer for the agent loop: a thin fetch wrapper bound to the
// lifeos-api base + workspace. Returns { ok, status, data } and only throws on
// a genuine network failure (so the gate can fail closed on it). Injectable in
// tests via opts.httpFn so no turn ever hits the network.
const DEFAULT_API_BASE = process.env.LIFEOS_API_URL || "http://127.0.0.1:8080";

export function createHttpFn(workspaceId, apiBase = DEFAULT_API_BASE) {
  return async function httpFn(method, path, body) {
    const res = await fetch(`${apiBase}${path}`, {
      method,
      headers: {
        "Content-Type": "application/json",
        "X-Workspace-Id": workspaceId,
      },
      body: body ? JSON.stringify(body) : undefined,
    });
    const text = await res.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    return { ok: res.ok, status: res.status, data };
  };
}
