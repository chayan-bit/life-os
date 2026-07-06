// Central API client + route registry for Life OS.
// Every backend route the system exposes (live or planned) is declared here so
// the frontend can both call it AND surface it in the API Explorer. The base URL
// is configurable via VITE_API_URL; it falls back to the local trusted host.

export const API_BASE =
  (import.meta.env && import.meta.env.VITE_API_URL) || 'http://127.0.0.1:8080';

// Self-hosted Nango Connect UI (infra/nango/docker-compose.yml CONNECT_UI_PORT).
export const NANGO_CONNECT_UI_URL =
  (import.meta.env && import.meta.env.VITE_NANGO_CONNECT_URL) || 'http://localhost:3009';

// Canonical localStorage keys for tenant + soft-auth state. See FRONTEND.md §1.
export const WORKSPACE_ID_KEY = 'life_os_workspace_id';
export const KEY_TOKEN_KEY = 'life_os_key_token';
// Refresh token backing session rotation (issue #100, docs/SECURITY.md §5).
// Only ever sent to /api/session/refresh and /api/logout - never attached
// to authHeaders(), so it can't leak onto ordinary API calls the way a
// key_token deliberately does.
export const REFRESH_TOKEN_KEY = 'life_os_refresh_token';
const DEFAULT_WORKSPACE_ID = 'default-personal-workspace';

// status: 'live'    -> implemented in lifeos-api (services/lifeos-api/src/)
//         'queued'  -> implemented: enqueues a job, returns 202 (service drains later)
//         'planned' -> route exists but honestly returns 501 until its phase ships
export const API_ROUTES = [
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/health',
    status: 'live',
    summary: 'Liveness probe for the canonical API.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/entity',
    status: 'live',
    summary: 'Create an entity (generic workspace + module + type + attrs row).',
    sample: { module: 'tasks', type: 'task', title: 'Ship overhaul', attrs: { priority: 'high' } },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/entity',
    status: 'live',
    summary: 'Query / list entities by workspace + module + type + status.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/entity/:id',
    status: 'live',
    summary: 'Fetch a single entity by id (workspace-scoped).',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'PATCH',
    path: '/api/entity/:id',
    status: 'live',
    summary: 'Update an entity (lifecycle / attrs). Emits an entity.updated event.',
    sample: { status: 'completed' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/annotation',
    status: 'live',
    summary: 'Create a workspace-scoped note/highlight/question/comment/link on an entity.',
    sample: { entity_id: 'topic-sockets', kind: 'comment', body: 'great point', anchor: { type: 'selection', quote: 'BSD sockets' } },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/annotation',
    status: 'live',
    summary: 'List annotations, filterable by entity_id and/or kind (workspace-scoped).',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'PATCH',
    path: '/api/annotation/:id',
    status: 'live',
    summary: 'Update an annotation body/kind/anchor/attrs. Emits an annotation.updated event.',
    sample: { body: 'edited note' },
  },
  {
    service: 'lifeos-api',
    method: 'DELETE',
    path: '/api/annotation/:id',
    status: 'live',
    summary: 'Delete an annotation (workspace-scoped). Emits an annotation.deleted event.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/edge',
    status: 'live',
    summary: 'Create a graph relation between entities (or to an external ref).',
    sample: { src_id: 'ent_a', dst_id: 'ent_b', rel: 'depends_on' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/edge',
    status: 'live',
    summary: 'List edges by src / dst / relation.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/event',
    status: 'live',
    summary: 'Append a domain / harness event. Append-only: no update or delete route exists.',
    sample: { type: 'study.review', actor: 'user' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/event',
    status: 'live',
    summary: 'Read the append-only event / run log.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/jobs',
    status: 'live',
    summary: 'Read the job queue lifeos-drain claims from.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/metrics',
    status: 'live',
    summary: 'SQL aggregation over events + entities for dashboards.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/module-request',
    status: 'live',
    summary: 'Record a self-extension request, log it, and enqueue a module_build job.',
    sample: { prompt: 'Add a habit-tracking module' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/register',
    status: 'live',
    summary: 'Register a new workspace (tenant) + user (real hashed password, issue #100) and mint a key_token + refresh_token.',
    sample: { email: 'me@example.com', name: 'Me', password: 'a-real-password', workspace_name: 'My Life OS' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/me',
    status: 'live',
    summary: 'The authenticated user from the bearer token claims, if any.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/workspace',
    status: 'live',
    summary: 'The resolved workspace row (name/plan).',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'PATCH',
    path: '/api/workspace',
    status: 'live',
    summary: 'Rename the workspace or change its plan.',
    sample: { name: 'My Life OS' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/agents',
    status: 'live',
    summary: 'List local agent CLIs detected on PATH (the /api/llm engines) + the default.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/llm',
    status: 'live',
    summary: 'Route a prompt to a local agent CLI (Claude Code / Gemini / ...) and return { text }.',
    sample: { system: 'You are a study assistant.', prompt: 'Explain CAP theorem.' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/connections',
    status: 'live',
    summary: 'List owned-credential connections (provider, handle, status). Never returns a token.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/connections/session',
    status: 'live',
    summary: "Mint a Nango Connect session token to run a provider's OAuth dance.",
    sample: { provider: 'google-mail' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/connections/complete',
    status: 'live',
    summary: 'Record a connection after the Nango Connect UI reports success.',
    sample: { connection_id: 'con_x_091', provider: 'google-mail' },
  },
  {
    service: 'lifeos-api',
    method: 'DELETE',
    path: '/api/connections/:id',
    status: 'live',
    summary: 'Revoke a connection with Nango and mark it disconnected.',
    sample: null,
  },
  {
    service: 'lifeos-ingest',
    method: 'POST',
    path: '/api/ingest',
    status: 'queued',
    summary: 'Enqueue media ingest (transcribe / caption / parse into segments). Returns 202.',
    sample: { uri: 's3://lifeos-vault/demo-reel.mp4', kind: 'video' },
  },
  {
    service: 'lifeos-pipelines',
    method: 'POST',
    path: '/api/pipeline/run',
    status: 'queued',
    summary: 'Enqueue a Life OS Action / pipeline DAG run. Returns 202.',
    sample: { pipeline: 'social-draft', input: {} },
  },
  {
    service: 'lifeos-pipelines',
    method: 'GET',
    path: '/api/pipeline/registry',
    status: 'live',
    summary: 'List registered pipelines and their DAG stages (issue #94) - static, no tenant scoping.',
    sample: null,
  },
  {
    service: 'lifeos-vcs',
    method: 'GET',
    path: '/api/vcs/history',
    status: 'live',
    summary: 'Content-addressed version history for a file entity.',
    sample: null,
  },
  {
    service: 'lifeos-vcs',
    method: 'POST',
    path: '/api/vcs/commit',
    status: 'live',
    summary: 'Commit a new content-addressed version (bytes uploaded base64, never a server-trusted path).',
    sample: { name: 'notes.txt', content_base64: '...', message: 'first version' },
  },
  {
    service: 'lifeos-vcs',
    method: 'GET',
    path: '/api/vcs/checkout',
    status: 'live',
    summary: 'Retrieve a file version’s raw bytes by entity id (+ optional historical blob_ref).',
    sample: null,
  },
  {
    service: 'lifeos-vcs',
    method: 'GET',
    path: '/api/vcs/diff',
    status: 'live',
    summary: 'Per-type semantic diff between two versions - real for text-backed types, honestly unsupported (named blocking issue) otherwise.',
    sample: null,
  },
  {
    service: 'lifeos-vcs',
    method: 'GET',
    path: '/api/vcs/refs',
    status: 'live',
    summary: 'List branches or tags in the workspace.',
    sample: null,
  },
  {
    service: 'lifeos-vcs',
    method: 'POST',
    path: '/api/vcs/branch',
    status: 'live',
    summary: 'Snapshot current workspace state and point a moving branch at it.',
    sample: { name: 'main' },
  },
  {
    service: 'lifeos-vcs',
    method: 'POST',
    path: '/api/vcs/tag',
    status: 'live',
    summary: 'Snapshot current workspace state and point a fixed tag at it (refuses to move once set).',
    sample: { name: 'v1' },
  },
  {
    service: 'lifeos-vcs',
    method: 'GET',
    path: '/api/vcs/snapshot',
    status: 'live',
    summary: 'Read a snapshot’s entity_id -> blob_ref manifest - "everything as it was" at that point.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/broker/positions',
    status: 'planned',
    summary: 'Read-only broker positions (501 until the trading phase). Order routes never exist.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/login',
    status: 'live',
    summary: 'Real login (issue #100): verifies a password and issues an access token + rotating refresh token.',
    sample: { email: 'you@example.com', password: 'your-password' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/session/refresh',
    status: 'live',
    summary: 'Rotates a refresh token: revokes it and issues a fresh access token + refresh token.',
    sample: { refresh_token: '<refresh_token>' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/logout',
    status: 'live',
    summary: 'Revokes one session.',
    sample: { refresh_token: '<refresh_token>' },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/marketplace/pubkey',
    status: 'live',
    summary: 'The marketplace platform ed25519 public key (issue #101). 501 until LIFEOS_MARKETPLACE_SIGNING_SEED is configured.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/marketplace/publish',
    status: 'live',
    summary: 'Structurally validates + ed25519-signs a module manifest and stores the package (issue #102).',
    sample: { module_id: 'reading', version: '1.0.0', manifest: { id: 'reading', version: '1.0.0' } },
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/marketplace/packages',
    status: 'live',
    summary: 'Browse published packages.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/marketplace/verify',
    status: 'live',
    summary: 'Verifies a manifest/signature/pubkey triple - a tampered manifest fails (issue #101).',
    sample: { manifest: {}, signature: '<sig>', pubkey: '<pubkey>' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/marketplace/install',
    status: 'live',
    summary: 'Re-verifies a stored package\'s signature and records a marketplace.installed event. The actual git-commit-as-install step is the Node scaffold layer\'s job.',
    sample: { package_id: '<package_id>' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/push/subscribe',
    status: 'live',
    summary: 'Stores a Web Push subscription (issue #103). Actual VAPID push delivery is deferred.',
    sample: { endpoint: 'https://push.example/...', keys: { p256dh: '...', auth: '...' } },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/push/unsubscribe',
    status: 'live',
    summary: 'Removes a Web Push subscription.',
    sample: { endpoint: 'https://push.example/...' },
  },
  {
    service: 'lifeos-api',
    method: 'POST',
    path: '/api/workspace/provision-db',
    status: 'live',
    summary: 'Provisions a dedicated Turso database for the workspace (issue #104). 501 until TURSO_PLATFORM_API_TOKEN/TURSO_ORG_SLUG are configured. No plan/quota gating.',
    sample: null,
  },
  {
    service: 'lifeos-api',
    method: 'GET',
    path: '/api/workspace/database',
    status: 'live',
    summary: 'The workspace\'s provisioned database name/url, if any. Never returns the auth token.',
    sample: null,
  },
];

// Tenant + soft-auth headers attached to every request, mirroring core/db.js's
// header shape (X-Workspace-Id always; Authorization only once a key_token
// exists). Without these the backend silently falls back to the default
// workspace, masking tenant bugs - see FRONTEND.md §1. Exported for the
// rare non-JSON fetch (raw blob bytes in lib/vcsApi.js).
export function authHeaders(json) {
  const headers = {};
  if (json) headers['Content-Type'] = 'application/json';
  headers['X-Workspace-Id'] = localStorage.getItem(WORKSPACE_ID_KEY) || DEFAULT_WORKSPACE_ID;
  const keyToken = localStorage.getItem(KEY_TOKEN_KEY);
  if (keyToken) headers['Authorization'] = `Bearer ${keyToken}`;
  return headers;
}

// Thin fetch wrapper. Returns { ok, data, error, offline }.
export async function apiCall(method, path, body) {
  try {
    const res = await fetch(`${API_BASE}${path}`, {
      method,
      headers: authHeaders(Boolean(body)),
      body: body ? JSON.stringify(body) : undefined,
    });
    const text = await res.text();
    let data = null;
    try { data = text ? JSON.parse(text) : null; } catch { data = text; }
    return { ok: res.ok, status: res.status, data, error: res.ok ? null : (data?.error || res.statusText), offline: false };
  } catch (e) {
    return { ok: false, status: 0, data: null, error: e.message, offline: true };
  }
}

// Ping a single route's health-style endpoint. Used by the API Explorer.
export async function pingRoute(route) {
  // Only GET routes with no required body are safely pingable; others report 'unknown'.
  if (route.method !== 'GET') return { reachable: null };
  const { ok, offline } = await apiCall('GET', route.path);
  return { reachable: offline ? false : ok };
}
