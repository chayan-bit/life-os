import React, { useState } from 'react';
import { Refine, useList } from '@refinedev/core';
import { Boxes, FlaskConical } from 'lucide-react';
import { refineDataProvider } from '../lib/refineDataProvider';
import GenericList from '../core/renderers/GenericList';

// Finding 55: this page is a proof-of-concept, not a shipped feature, and is
// the sole consumer of @refinedev/core in the app - already code-split via
// App.jsx's `lazy(() => import('./pages/RefineDemo'))`, so the dependency
// never lands in the main bundle. Follow-up (needs App.jsx + Layout.jsx,
// which this fix does not touch): drop the "/refine-demo" route and its nav
// entry from production once the dataProvider proof is no longer needed.

// Proof that refineDataProvider works end-to-end against the live API: a
// Refine `useList` call rendering real `tasks/task` entities. See issue
// "Frontend: Refine dataProvider over the generic-entity API".
function TaskList() {
  const [pageSize] = useState(10);
  const { data, isLoading, isError, error } = useList({
    resource: 'tasks/task',
    pagination: { current: 1, pageSize },
  });

  if (isLoading) return <p className="text-xs text-neo-text-muted">Loading via Refine useList…</p>;
  if (isError) return <p className="text-xs text-neo-red">Refine query failed: {error?.message}</p>;

  const rows = data?.data || [];
  return (
    <GenericList
      entities={rows}
      display={{ title: 'title', badge: 'status' }}
      emptyLabel="No tasks/task entities yet - create one from the Database or Modules page, then reload this view."
    />
  );
}

export default function RefineDemo() {
  return (
    <div className="flex flex-col gap-6">
      <div className="neo-surface neo-border-thick neo-shadow p-3 bg-neo-yellow flex items-center gap-2 text-neo-text">
        <FlaskConical size={16} className="shrink-0" />
        <span className="neo-label-sm font-bold uppercase">
          Demo / proof-of-concept - not a production feature. Kept for the Refine dataProvider integration proof.
        </span>
      </div>

      <div className="neo-surface neo-border-thick neo-shadow p-6 bg-neo-surface">
        <h2 className="neo-title-md mb-2 flex items-center gap-2">
          <Boxes size={22} />
          Refine DataProvider Proof
        </h2>
        <p className="neo-body-md text-neo-text-muted">
          This list is rendered by Refine's <code>useList</code> hook through{' '}
          <code>frontend/src/lib/refineDataProvider.js</code>, a generic dataProvider
          mapping any <code>module/type</code> resource onto <code>/api/entity</code>{' '}
          (and <code>/api/edge</code> for relations). Adding a new generic view
          (list/board/table/calendar/...) is now a matter of pointing a Refine hook
          at a resource string - no per-module backend wiring.
        </p>
      </div>

      <div className="neo-surface neo-border-thick neo-shadow p-5 bg-neo-surface">
        <h3 className="neo-label-md mb-3">Resource: tasks/task</h3>
        <Refine dataProvider={refineDataProvider} resources={[{ name: 'tasks/task' }]}>
          <TaskList />
        </Refine>
      </div>
    </div>
  );
}
