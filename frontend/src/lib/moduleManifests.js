// Static registry of day-1 module manifests (docs/MODULES.md §1). These are
// shipped with the app (not hot-installed via self-extension - that's
// moduleRegistry.js's job for AI-built modules), but follow exactly the same
// manifest shape so they render through the same generic renderers with zero
// bespoke view code.
//
// Each manifest: { id, name, icon, entityTypes: { <type>: {label, plural,
// display} }, views: [{id, label, kind, type, dateField?, groupBy?}] }.
// `kind` selects which core/renderers/Generic* component renders the view.

export const LEARNING_MANIFEST = {
  id: 'learning',
  name: 'Learning / Study',
  icon: '🧠',
  entityTypes: {
    domain: {
      label: 'Domain',
      plural: 'Domains',
      display: { title: 'title', subtitle: 'tagline', badge: 'icon' },
    },
    topic: {
      label: 'Topic',
      plural: 'Topics',
      display: { title: 'title', subtitle: 'level', badge: 'status' },
    },
    subtopic: {
      label: 'Subtopic',
      plural: 'Subtopics',
      display: { title: 'title' },
    },
    resource: {
      label: 'Resource',
      plural: 'Resources',
      display: { title: 'title', subtitle: 'url', badge: 'kind' },
    },
    gap: {
      label: 'Gap',
      plural: 'Gaps',
      display: { title: 'title', subtitle: 'description', badge: 'severity' },
    },
    question: {
      label: 'Question',
      plural: 'Questions',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'atlas', label: 'Atlas (domains)', kind: 'list', type: 'domain' },
    { id: 'topics', label: 'Topics', kind: 'table', type: 'topic', columns: [
      { key: 'title', label: 'Topic', truncate: true },
      { key: 'level', label: 'Level' },
      { key: 'status', label: 'Status', editable: true },
    ] },
    { id: 'gaps', label: 'Gaps inbox', kind: 'list', type: 'gap' },
    { id: 'due', label: "What's due", kind: 'calendar', type: 'topic', dateField: 'next_due' },
    { id: 'graph', label: 'Knowledge Graph (cross-domain)', kind: 'graph', type: 'topic' },
  ],
};

export const TASKS_MANIFEST = {
  id: 'tasks',
  name: 'Tasks / Productivity',
  icon: '✅',
  entityTypes: {
    task: {
      label: 'Task',
      plural: 'Tasks',
      display: { title: 'title', subtitle: 'label', badge: 'status' },
    },
    project: {
      label: 'Project',
      plural: 'Projects',
      display: { title: 'title', subtitle: 'description' },
    },
    schedule_block: {
      label: 'Schedule Block',
      plural: 'Schedule Blocks',
      display: { title: 'title', subtitle: 'start' },
    },
  },
  views: [
    { id: 'board', label: 'Board', kind: 'board', type: 'task', groupBy: 'status', columns: ['DRAFT', 'IN_PROGRESS', 'REVIEW', 'COMPLETED'] },
    { id: 'today', label: 'Today', kind: 'list', type: 'task', filter: { field: 'due', onOrBefore: 'today' } },
    { id: 'calendar', label: 'Calendar', kind: 'calendar', type: 'task', dateField: 'due' },
  ],
};

export const CODING_MANIFEST = {
  id: 'coding',
  name: 'Coding / Projects',
  icon: '🛠️',
  entityTypes: {
    project: {
      label: 'Project',
      plural: 'Projects',
      display: { title: 'title' },
    },
    repo: {
      label: 'Repo',
      plural: 'Repos',
      // ci_state is local-only until GitHub data flows in via Nango (Phase 3)
      display: { title: 'title', subtitle: (e) => e.attrs?.remote, badge: 'status' },
    },
    gap: {
      label: 'Gap',
      plural: 'Gaps',
      display: { title: 'title', subtitle: 'description' },
    },
    ci_run: {
      label: 'CI Run',
      plural: 'CI Runs',
      display: { title: 'title', badge: 'status' },
    },
    review: {
      label: 'Review',
      plural: 'Reviews',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'status', label: 'Status', kind: 'table', type: 'repo', columns: [
      { key: 'title', label: 'Repo' },
      { key: 'status', label: 'Status', editable: true },
      { key: 'default_branch', label: 'Branch' },
      { key: 'ci_state', label: 'CI' },
    ] },
    { id: 'board', label: 'Blocked / Active / Done', kind: 'board', type: 'repo', groupBy: 'status', columns: ['dirty', 'clean', 'blocked'] },
  ],
};

// Trading is read-only for any agent/bot by hard architectural rule
// (docs/SECURITY.md §1): there is no place/modify/cancel/GTT tool anywhere
// in the closed ACTION_TOOLS registry (lib/agentActions.js), and
// broker-guard fails closed on any such attempt at the hook layer. A
// `proposed_order` is a draft-only entity; turning it into a real order
// requires a separate, human-typed-confirmation executor that does not
// exist in this app at all (Phase 6) - the manifest below has no tool, no
// agentTool, and no button anywhere that could place one.
export const TRADING_MANIFEST = {
  id: 'trading',
  name: 'Trading',
  icon: '📈',
  entityTypes: {
    trade: {
      label: 'Trade',
      plural: 'Trades',
      display: { title: (e) => `${e.attrs?.symbol || '?'} ${e.attrs?.side || ''}`, subtitle: 'status', badge: (e) => e.attrs?.r_multiple != null ? `R ${e.attrs.r_multiple}` : null },
    },
    setup: {
      label: 'Setup / Playbook',
      plural: 'Setups',
      display: { title: 'title', subtitle: 'description' },
    },
    proposed_order: {
      label: 'Proposed Order (draft only - never auto-executes)',
      plural: 'Proposed Orders',
      display: { title: (e) => `${e.attrs?.symbol || '?'} ${e.attrs?.side || ''} x${e.attrs?.qty || '?'}`, badge: 'status' },
    },
  },
  views: [
    { id: 'journal', label: 'Journal', kind: 'table', type: 'trade', columns: [
      { key: 'symbol', label: 'Symbol' },
      { key: 'side', label: 'Side' },
      { key: 'entry', label: 'Entry' },
      { key: 'exit', label: 'Exit' },
      { key: 'stop', label: 'Stop' },
      { key: 'target', label: 'Target' },
      { key: 'r_multiple', label: 'R' },
      { key: 'pnl', label: 'PnL' },
      { key: 'emotion', label: 'Emotion', editable: true },
    ] },
    { id: 'setups', label: 'Setups', kind: 'list', type: 'setup' },
    { id: 'proposed', label: 'Proposed Orders (approve in Telegram - never auto-runs)', kind: 'list', type: 'proposed_order' },
    { id: 'equity', label: 'Equity Curve', kind: 'metric', metric: 'equity_curve' },
  ],
  metrics: [
    { id: 'equity_curve', source: 'events', where: { type: 'trade.closed' }, agg: 'sum:pnl', bucket: 'day', viz: 'line', cumulative: true },
  ],
};

// Publishing is always gated (docs/SECURITY.md §2): drafting any post/reply/
// dm goes through the single closed `draft.create` tool (lib/agentActions.js,
// #33), which is already classified `gated` regardless of which module/type
// it's drafting for - there is no separate "social.publish" tool registered
// anywhere, so a draft can only become published via a human decision (see
// Modules.jsx's decideDraft, #32). Account linking (Instagram/X/Reddit/Slack/
// WhatsApp via Nango) is explicitly deferred to Phase 3 per this issue.
export const SOCIAL_MANIFEST = {
  id: 'social',
  name: 'Social',
  icon: '💬',
  entityTypes: {
    social_account: {
      label: 'Social Account',
      plural: 'Accounts',
      display: { title: (e) => e.attrs?.handle, subtitle: 'provider' },
    },
    post: {
      label: 'Post',
      plural: 'Posts',
      display: { title: 'title', subtitle: (e) => e.attrs?.scheduled_for, badge: 'status' },
    },
    reply: {
      label: 'Reply',
      plural: 'Replies',
      display: { title: 'title', badge: 'status' },
    },
    dm: {
      label: 'DM',
      plural: 'DMs',
      display: { title: 'title', badge: 'status' },
    },
    mention: {
      label: 'Mention',
      plural: 'Mentions',
      display: { title: 'title', subtitle: (e) => e.attrs?.from },
    },
    thread: {
      label: 'Thread',
      plural: 'Threads',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'inbox', label: 'Inbox (mentions + DMs)', kind: 'list', type: 'mention' },
    // 'pending_approval' matches the status draft.create now writes (server/
    // agent/actionRegistry.js + frontend/src/lib/agentActions.js + worker/src/
    // approvals.ts) - was 'drafted', which made agent-created posts invisible
    // on this board.
    { id: 'posts', label: 'Posts', kind: 'board', type: 'post', groupBy: 'status', columns: ['pending_approval', 'published', 'rejected'] },
    { id: 'accounts', label: 'Accounts (link via Integrations - Nango, Phase 3)', kind: 'list', type: 'social_account' },
  ],
};

// Publishing content is outward (it `publishes_to` a Social account), so it
// stays gated through the same draft.create -> human decision path Social
// uses (#43) - this manifest declares no publish tool of its own.
export const MARKETING_MANIFEST = {
  id: 'marketing',
  name: 'Marketing',
  icon: '📣',
  entityTypes: {
    campaign: {
      label: 'Campaign',
      plural: 'Campaigns',
      display: { title: 'title', subtitle: (e) => `${e.attrs?.start || '?'} → ${e.attrs?.end || '?'}`, badge: 'status' },
    },
    content: {
      label: 'Content',
      plural: 'Content',
      display: { title: 'title', subtitle: (e) => e.attrs?.channel, badge: 'status' },
    },
    audience: {
      label: 'Audience / Segment',
      plural: 'Audiences',
      display: { title: 'title' },
    },
    lead: {
      label: 'Lead',
      plural: 'Leads',
      display: { title: 'title', subtitle: (e) => e.attrs?.email, badge: 'status' },
    },
    channel: {
      label: 'Channel',
      plural: 'Channels',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'calendar', label: 'Content Calendar', kind: 'calendar', type: 'content', dateField: 'scheduled_for' },
    { id: 'campaigns', label: 'Campaigns', kind: 'list', type: 'campaign' },
    { id: 'leads', label: 'Leads', kind: 'table', type: 'lead', columns: [
      { key: 'title', label: 'Lead' },
      { key: 'email', label: 'Email' },
      { key: 'status', label: 'Status', editable: true },
    ] },
    { id: 'funnel', label: 'Funnel', kind: 'metric', metric: 'campaign_funnel' },
  ],
  metrics: [
    {
      id: 'campaign_funnel',
      source: 'events',
      viz: 'funnel',
      stages: [
        { label: 'Drafted', where: { type: 'content.drafted' } },
        { label: 'Sent', where: { type: 'content.sent' } },
        { label: 'Campaign launched', where: { type: 'campaign.launched' } },
      ],
    },
  ],
};

// mcp-figma (read+write) and mcp-higgsfield (generation) are loaded
// on-demand via mcp-multiplexer when actually invoked, never mounted
// always-on (docs/INTEGRATIONS.md token-discipline rule) - this manifest has
// no UI of its own that calls them yet (that needs an agent/console surface,
// not a view), so it stays an honest gallery + library over whatever assets
// already exist. Per-type semantic diff for design files is lifeos-vcs work,
// explicitly deferred to Phase 6.
export const DESIGN_MANIFEST = {
  id: 'design',
  name: 'Design',
  icon: '🎨',
  entityTypes: {
    design_file: {
      label: 'Design File (Figma)',
      plural: 'Design Files',
      display: { title: 'title', subtitle: (e) => e.attrs?.figma_url },
    },
    component: {
      label: 'Component',
      plural: 'Components',
      display: { title: 'title', subtitle: (e) => e.attrs?.derived_from },
    },
    token: {
      label: 'Token',
      plural: 'Tokens',
      display: { title: 'title', subtitle: (e) => e.attrs?.value },
    },
    asset: {
      label: 'Asset',
      plural: 'Assets',
      display: { title: 'title', badge: (e) => e.attrs?.kind },
    },
    brief: {
      label: 'Brief',
      plural: 'Briefs',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'gallery', label: 'Assets', kind: 'gallery', type: 'asset', mediaField: 'blob_ref' },
    { id: 'library', label: 'Component Library', kind: 'table', type: 'component', columns: [
      { key: 'title', label: 'Component' },
      { key: 'derived_from', label: 'Derived From' },
    ] },
    { id: 'files', label: 'Design Files', kind: 'list', type: 'design_file' },
  ],
};

// Reads (`gmail.sync|read|search`) are unconditionally free; `gmail.send`
// stays gated through the same draft.create -> human-approve path every
// other outward write uses (services/lifeos-api/src/routes/gmail.rs) - this
// manifest declares a `sync` action (materializes entities via
// POST /api/gmail/sync) but no send/publish tool of its own.
export const EMAIL_MANIFEST = {
  id: 'email',
  name: 'Email',
  icon: '📧',
  sync: { label: 'Sync inbox', path: '/api/gmail/sync' },
  entityTypes: {
    email_thread: {
      label: 'Thread',
      plural: 'Threads',
      display: { title: 'title' },
    },
    email: {
      label: 'Email',
      plural: 'Emails',
      display: { title: 'title', subtitle: (e) => e.attrs?.from, badge: (e) => e.attrs?.unread ? 'unread' : null },
    },
    contact: {
      label: 'Contact',
      plural: 'Contacts',
      display: { title: 'title', subtitle: (e) => e.attrs?.email },
    },
    mail_label: {
      label: 'Label',
      plural: 'Labels',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'inbox', label: 'Inbox', kind: 'list', type: 'email' },
    { id: 'triage', label: 'Triage', kind: 'board', type: 'email', groupBy: 'status', columns: ['now', 'later', 'done'] },
    { id: 'threads', label: 'Threads', kind: 'list', type: 'email_thread' },
  ],
};

// Reads (`cal.sync|read`) are unconditionally free; `cal.create|move` stay
// gated through the same draft.create -> human-approve path every other
// outward write uses (services/lifeos-api/src/routes/calendar.rs) - this
// manifest declares a `sync` action (materializes entities via
// POST /api/calendar/sync) but the agenda/calendar views are read-only, so
// there is no drag-to-move affordance for `GenericCalendar` to expose.
export const CALENDAR_MANIFEST = {
  id: 'calendar',
  name: 'Calendar',
  icon: '📅',
  sync: { label: 'Sync events', path: '/api/calendar/sync' },
  entityTypes: {
    calendar_event: {
      label: 'Event',
      plural: 'Events',
      display: { title: 'title', subtitle: (e) => e.attrs?.location, badge: (e) => e.attrs?.start?.slice(11, 16) },
    },
  },
  views: [
    { id: 'calendar', label: 'Calendar', kind: 'calendar', type: 'calendar_event', dateField: 'start' },
    { id: 'agenda', label: 'Agenda', kind: 'list', type: 'calendar_event' },
  ],
};

// `drive.sync|read` and local `file.commit` are free; `drive.upload|share`
// stay gated through the same draft.create -> human-approve path every
// other outward write uses (services/lifeos-api/src/routes/{drive,files}.rs).
// Version history (docs/VERSIONING.md) is a query over `events` for a given
// entity id, not a bespoke field here - the table view surfaces `version_no`
// (the latest version number) so a change is visible without a dedicated
// timeline UI, which is deferred.
export const FILES_MANIFEST = {
  id: 'files',
  name: 'Files',
  icon: '🗂️',
  sync: { label: 'Sync Drive', path: '/api/drive/sync' },
  entityTypes: {
    file: {
      label: 'File',
      plural: 'Files',
      display: { title: 'name', subtitle: (e) => e.attrs?.mime, badge: (e) => e.attrs?.version_no ? `v${e.attrs.version_no}` : null },
    },
    folder: {
      label: 'Folder',
      plural: 'Folders',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'browse', label: 'Browse', kind: 'table', type: 'file', columns: [
      { key: 'name', label: 'Name', truncate: true },
      { key: 'mime', label: 'Type' },
      { key: 'version_no', label: 'Version' },
      { key: 'parent_folder', label: 'Folder' },
    ] },
  ],
};

// `note.sync` (read pages/databases in) is free; the "edits propagate back"
// half is `POST /api/notion/push`, gated the same way as every other
// outward write - reachable from EntityDetailPanel's "Push to Notion"
// button (shown for `notion.note` entities), not a manifest-level action,
// since it's a per-entity push rather than a bulk sync.
export const NOTION_MANIFEST = {
  id: 'notion',
  name: 'Notion',
  icon: '📝',
  sync: { label: 'Sync from Notion', path: '/api/notion/sync' },
  entityTypes: {
    note: {
      label: 'Note',
      plural: 'Notes',
      display: { title: 'title' },
    },
    notion_page: {
      label: 'Notion Page',
      plural: 'Notion Pages',
      display: { title: 'title', subtitle: (e) => e.attrs?.last_edited_time },
    },
    notion_db: {
      label: 'Notion Database',
      plural: 'Notion Databases',
      display: { title: 'title' },
    },
  },
  views: [
    { id: 'notes', label: 'Notes', kind: 'list', type: 'note' },
    { id: 'pages', label: 'Mirrored Pages', kind: 'list', type: 'notion_page' },
    { id: 'databases', label: 'Databases', kind: 'list', type: 'notion_db' },
  ],
};

// `slack.sync|read` are free; `slack.post` stays gated through the same
// draft.create -> human-approve path every other outward write uses
// (services/lifeos-api/src/routes/slack.rs) - Slack is a second
// capture/notify surface alongside Telegram, not an outbound channel the
// agent can post to unsupervised.
export const SLACK_MANIFEST = {
  id: 'slack',
  name: 'Slack',
  icon: '💬',
  sync: { label: 'Sync channels', path: '/api/slack/sync' },
  entityTypes: {
    channel: {
      label: 'Channel',
      plural: 'Channels',
      display: { title: 'title' },
    },
    message: {
      label: 'Message',
      plural: 'Messages',
      display: { title: 'title', subtitle: (e) => e.attrs?.user },
    },
  },
  views: [
    { id: 'channels', label: 'Channels', kind: 'list', type: 'channel' },
    { id: 'messages', label: 'Messages', kind: 'list', type: 'message' },
  ],
};

// `reading.save` and `reading.highlight` are both free (no Nango/OAuth
// credential needed - fetching a public URL needs no owned credentials,
// services/lifeos-api/src/routes/reading.rs). There is no bulk sync button
// here (unlike Email/Calendar/Files/Notion/Slack) because Reading has no
// external inbox to poll - articles are saved one URL at a time, so this
// manifest declares no `sync` block.
export const READING_MANIFEST = {
  id: 'reading',
  name: 'Reading',
  icon: '📚',
  entityTypes: {
    article: {
      label: 'Article',
      plural: 'Articles',
      display: { title: 'title', subtitle: (e) => e.attrs?.url, badge: 'read_state' },
    },
    highlight: {
      label: 'Highlight',
      plural: 'Highlights',
      display: { title: (e) => e.attrs?.quote, subtitle: (e) => e.attrs?.color },
    },
    source: {
      label: 'Source',
      plural: 'Sources',
      display: { title: (e) => e.attrs?.domain },
    },
  },
  views: [
    { id: 'articles', label: 'Articles', kind: 'list', type: 'article' },
    { id: 'highlights', label: 'Highlights', kind: 'list', type: 'highlight' },
    { id: 'sources', label: 'Sources', kind: 'list', type: 'source' },
  ],
};

// `travel.book` (actually purchasing a flight/hotel) is gated the same way
// as every other outward write (services/lifeos-api/src/routes/travel.rs) -
// this manifest declares no book/purchase tool of its own. Trip/leg/place
// are plain user-authored entities (no external provider to sync from), so
// unlike Email/Calendar/Files/Notion/Slack there is no bulk-sync button for
// them; the one manifest-level action Travel has is deriving `booking`
// entities from already-synced email via the shared sync-button mechanism.
export const TRAVEL_MANIFEST = {
  id: 'travel',
  name: 'Travel',
  icon: '✈️',
  sync: { label: 'Parse confirmation emails', path: '/api/travel/parse-emails' },
  entityTypes: {
    trip: {
      label: 'Trip',
      plural: 'Trips',
      display: { title: 'title', subtitle: (e) => `${e.attrs?.start || '?'} → ${e.attrs?.end || '?'}`, badge: 'status' },
    },
    leg: {
      label: 'Leg',
      plural: 'Legs',
      display: { title: 'title', subtitle: (e) => e.attrs?.kind, badge: (e) => e.attrs?.start?.slice(0, 10) },
    },
    booking: {
      label: 'Booking',
      plural: 'Bookings',
      display: { title: (e) => e.attrs?.provider || 'Booking', subtitle: (e) => e.attrs?.confirmation, badge: (e) => e.attrs?.cost },
    },
    place: {
      label: 'Place',
      plural: 'Places',
      display: { title: 'title', subtitle: (e) => e.attrs?.category },
    },
  },
  views: [
    { id: 'trips', label: 'Trips', kind: 'list', type: 'trip' },
    { id: 'timeline', label: 'Timeline', kind: 'timeline', type: 'leg', dateField: 'start' },
    { id: 'map', label: 'Map', kind: 'map', type: 'place', latField: 'lat', lngField: 'lng' },
    { id: 'bookings', label: 'Bookings', kind: 'table', type: 'booking', columns: [
      { key: 'provider', label: 'Provider' },
      { key: 'confirmation', label: 'Confirmation' },
      { key: 'cost', label: 'Cost' },
    ] },
  ],
};

export const MODULE_MANIFESTS = {
  learning: LEARNING_MANIFEST,
  tasks: TASKS_MANIFEST,
  coding: CODING_MANIFEST,
  trading: TRADING_MANIFEST,
  social: SOCIAL_MANIFEST,
  marketing: MARKETING_MANIFEST,
  design: DESIGN_MANIFEST,
  email: EMAIL_MANIFEST,
  calendar: CALENDAR_MANIFEST,
  files: FILES_MANIFEST,
  notion: NOTION_MANIFEST,
  slack: SLACK_MANIFEST,
  reading: READING_MANIFEST,
  travel: TRAVEL_MANIFEST,
};

export function getManifest(id) {
  return MODULE_MANIFESTS[id];
}
