// Agent Control Plane: the closed, typed action-tool registry the in-app
// agent must go through to actuate anything (docs/AGENT-CONTROL.md §1-2).
// Hard rule: the four protected domains (VCS rewrite, security/gating
// config, OAuth/connection creation, secret reads) have NO entry here at
// all - not "forbidden", *absent* - so there is no tool object an agent
// could even reference for them. Anything not absent is allowed or gated.
import { apiCall } from './api';

// Every name that would actuate a protected domain. Listed explicitly (not
// inferred from ACTION_TOOLS) so adding a new legitimate tool can never
// accidentally collide with - or be confused for - a protected one.
export const PROTECTED_TOOLS = Object.freeze([
  'vcs.rewrite',
  'vcs.branchForce',
  'vcs.gc',
  'vcs.deleteVersion',
  'security.configure',
  'security.setGating',
  'connection.create',
  'connection.revoke',
  'secret.read',
  'secret.write',
]);

// tool -> { classification: 'allowed' | 'gated', run(args) -> Promise<apiCall result> }
// `allowed` = reversible + internal, auto-applies. `gated` = outward/
// irreversible, requires human approval before `run` is ever invoked (the
// approval workflow itself is a separate issue; this registry only refuses
// to auto-execute gated tools).
export const ACTION_TOOLS = Object.freeze({
  'entity.create': {
    classification: 'allowed',
    run: (args) => apiCall('POST', '/api/entity', args),
  },
  'entity.update': {
    classification: 'allowed',
    run: (args) => apiCall('PATCH', `/api/entity/${args.id}`, args.patch),
  },
  'edge.create': {
    classification: 'allowed',
    run: (args) => apiCall('POST', '/api/edge', args),
  },
  // Drafting content for outward channels (social/email/etc.) is gated even
  // though the draft entity itself is internal - docs/AGENT-CONTROL.md §3
  // calls this out explicitly ("draft.create (gated, since publishing is
  // outward)"), so approval happens before the draft is even written. The
  // status default mirrors the backend draft.create tool and worker/src/
  // approvals.ts, which key the approval queue off 'pending_approval' - a
  // frontend-created draft with any other default status would be invisible
  // to that queue.
  'draft.create': {
    classification: 'gated',
    run: (args) => apiCall('POST', '/api/entity', { ...args, status: args.status || 'pending_approval' }),
  },
  'view.configure': {
    classification: 'allowed',
    // No backend route yet (saved views are a later epic) - persists to
    // localStorage so the tool is real today rather than a stub that lies
    // about succeeding.
    run: (args) => {
      localStorage.setItem(`life_os_view_${args.id}`, JSON.stringify(args.config));
      return Promise.resolve({ ok: true, data: args, error: null, offline: false });
    },
  },
  'dashboard.arrange': {
    classification: 'allowed',
    run: (args) => {
      localStorage.setItem('life_os_dashboard_layout', JSON.stringify(args.layout));
      return Promise.resolve({ ok: true, data: args, error: null, offline: false });
    },
  },
  'navigate': {
    classification: 'allowed',
    // The caller (CommandBar/AIConsole) supplies the actual router navigate
    // function; this registry only validates the action shape exists.
    run: (args) => Promise.resolve({ ok: true, data: args, error: null, offline: false }),
  },
  // Allowed: the backend actionRegistry.js is the enforcement authority for
  // this route, and pipeline stages that act outwardly gate themselves via
  // draft.create internally, so pipeline.run itself needs no separate gate.
  'pipeline.run': {
    classification: 'allowed',
    run: (args) => apiCall('POST', '/api/pipeline/run', args),
  },
  'module.requestBuild': {
    classification: 'allowed',
    run: (args) => apiCall('POST', '/api/module-request', args),
  },
  'search': {
    classification: 'allowed',
    run: (args) => apiCall('GET', `/api/search?${new URLSearchParams(args).toString()}`),
  },
});

export function classifyAction(tool) {
  if (PROTECTED_TOOLS.includes(tool)) return 'forbidden';
  if (ACTION_TOOLS[tool]) return ACTION_TOOLS[tool].classification;
  return 'forbidden'; // unknown tool = no tool = forbidden by default (closed set)
}

async function logDenied(tool, args) {
  await apiCall('POST', '/api/event', {
    type: 'action.denied',
    actor: 'agent',
    attrs: { tool, args },
  });
}

// Computes the reverse action for a tool, given the args that were applied
// (plus a captured "before" state on `args.__before` for entity.update) and
// the run() result. Returns null when the tool genuinely has no inverse in
// the closed ACTION_TOOLS registry - reported honestly, never fabricated.
function computeReverse(tool, args, result) {
  if (tool === 'entity.update') {
    if (!args.__before) return null;
    const reversePatch = {};
    for (const key of Object.keys(args.patch || {})) {
      reversePatch[key] = args.__before[key] ?? null;
    }
    return { tool: 'entity.update', args: { id: args.id, patch: reversePatch } };
  }
  if (tool === 'entity.create' || tool === 'draft.create') {
    const id = result?.data?.id;
    if (!id) return null;
    // No hard-delete exists anywhere (entities are lifecycle-managed) - the
    // honest reverse of a create is a soft-revert via status, not a delete.
    return { tool: 'entity.update', args: { id, patch: { status: 'undone' } } };
  }
  // edge.create has no edge.update/edge.delete tool in the closed registry;
  // view.configure/dashboard.arrange/navigate/search/module.requestBuild/
  // pipeline.run have no safe generic inverse either - honestly non-reversible.
  return null;
}

async function logApplied(tool, args, result, reverseAction, planId) {
  await apiCall('POST', '/api/event', {
    type: 'action.applied',
    actor: 'agent',
    entity_id: result?.data?.id,
    attrs: { tool, args, reverse_action: reverseAction, plan_id: planId || null },
  });
}

async function logUndone(originalEventId, tool, args, result) {
  await apiCall('POST', '/api/event', {
    type: 'action.undone',
    actor: 'agent',
    entity_id: result?.data?.id,
    attrs: { tool, args, undoes_event_id: originalEventId },
  });
}

// The single chokepoint every agent action must pass through. Returns
// { status: 'forbidden' | 'pending_approval' | 'applied' | 'failed', ... }
// - never executes a gated tool without explicit `approved: true`, and never
// even looks up a protected tool name beyond classifying it as forbidden.
// `planId` groups multi-step ActionPlan applications so a whole batch can be
// undone atomically later (issue #36).
export async function executeAction({ tool, args }, { approved = false, planId = null } = {}) {
  const classification = classifyAction(tool);

  if (classification === 'forbidden') {
    await logDenied(tool, args);
    return { status: 'forbidden', tool, reason: `'${tool}' has no action tool - this domain is hard-denied (docs/AGENT-CONTROL.md §1).` };
  }

  if (classification === 'gated' && !approved) {
    return { status: 'pending_approval', tool, reason: `'${tool}' is gated - outward/irreversible actions require human approval before they run.` };
  }

  // Capture "before" state for entity.update so the reverse patch reflects
  // real prior values, not a guess.
  let before = null;
  if (tool === 'entity.update' && args?.id) {
    const { ok, data } = await apiCall('GET', `/api/entity/${args.id}`);
    if (ok) before = data;
  }

  const result = await ACTION_TOOLS[tool].run(args);
  if (!result.ok) {
    return { status: 'failed', tool, error: result.error };
  }
  const reverseAction = computeReverse(tool, { ...args, __before: before }, result);
  await logApplied(tool, args, result, reverseAction, planId);
  return { status: 'applied', tool, data: result.data, reverseAction };
}

// Re-invokes an applied event's stored reverse action as a NEW forward
// action - history is never rewritten; undo is itself an `action.undone`
// event referencing the original (docs/AGENT-CONTROL.md). Always pre-
// approved: the reverse action was already vetted when its forward
// counterpart was classified and applied.
export async function undoAction(appliedEvent) {
  const reverseAction = appliedEvent.attrs?.reverse_action;
  if (!reverseAction) {
    return { status: 'not_reversible', reason: 'This action has no stored reverse - it was either non-reversible by design or applied before undo support existed.' };
  }
  const result = await ACTION_TOOLS[reverseAction.tool].run(reverseAction.args);
  if (!result.ok) return { status: 'failed', error: result.error };
  await logUndone(appliedEvent.id, reverseAction.tool, reverseAction.args, result);
  return { status: 'undone', data: result.data };
}

// Undoes every action in a plan atomically in reverse order (last-applied,
// first-undone) so partial dependencies between steps unwind safely.
export async function undoPlan(appliedEvents) {
  const ordered = [...appliedEvents].sort((a, b) => b.ts - a.ts);
  const results = [];
  for (const ev of ordered) {
    results.push({ event: ev, result: await undoAction(ev) });
  }
  return results;
}
