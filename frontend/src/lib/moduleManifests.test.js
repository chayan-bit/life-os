// docs/MODULES.md promises Learning a cross-domain graph view ("graph
// (cross-domain)"), but LEARNING_MANIFEST had no `kind: 'graph'` view - a
// scaffolded module could declare one (server/validators/structural.js
// already accepts it) while the shipped Learning manifest itself couldn't
// render one. Guard the fix so it can't silently regress.
import { describe, expect, it } from 'vitest';
import { LEARNING_MANIFEST } from './moduleManifests';

describe('LEARNING_MANIFEST', () => {
  it('declares a graph view', () => {
    const graphView = LEARNING_MANIFEST.views.find((view) => view.id === 'graph');
    expect(graphView).toBeTruthy();
    expect(graphView.kind).toBe('graph');
  });
});
