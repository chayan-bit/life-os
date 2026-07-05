// Single source of truth for the set of view `kind`s a generic renderer
// exists for (docs/SELF-EXTENSION-V2.md T1, #133). Plain, dependency-free JS
// - no React/JSX - so both the frontend (ModuleManifestPage.jsx's
// KIND_RENDERERS component map) and the server (structural.js's validator,
// which runs under plain Node ESM, no bundler) can import the exact same
// array without either side drifting out of sync with the other.
//
// A drift guard (ModuleManifestPage.test.jsx) asserts
// `Object.keys(KIND_RENDERERS)` (sorted) equals this array (sorted), so a
// new renderer registered on one side without the other fails the test suite
// instead of silently going stale.
export const RENDERER_KINDS = [
  'list',
  'table',
  'board',
  'calendar',
  'gallery',
  'timeline',
  'map',
  'graph',
];
