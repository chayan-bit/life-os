import React, { useEffect, useMemo, useState } from 'react';
import {
  Store, UploadCloud, DownloadCloud, ShieldCheck, ShieldAlert, ShieldQuestion,
  RefreshCw, Search, History, X, AlertTriangle, CheckCircle2,
} from 'lucide-react';
import { apiCall } from '../lib/api';

// Module marketplace (issues #101/#102/#147, docs/PLATFORM-SYSTEMS.md).
// Browse/search published packages; open a package for its manifest preview,
// version history and signature status; one-click install that the server
// re-validates through the Tier-0 validator chain before activating (a
// tampered or structurally-invalid bundle is rejected). Publishing is outward,
// so it is human-gated: publishing an installed module creates a draft that
// lands in the approval inbox, never a direct publish.

const MANIFEST_ENTITY_QUERY = '/api/entity?module=system&type=module_manifest&limit=2000';

// A short, stable fingerprint of a publisher key for the card/badge.
function shortKey(pubkey) {
  return pubkey ? `${pubkey.slice(0, 12)}...` : 'unsigned';
}

// The signature badge for a package: unknown until /verify answers, then
// verified (green) or unverified (red) - never a blank "trust me".
function SignatureBadge({ status }) {
  if (status === 'verified') {
    return (
      <span className="neo-label-sm flex items-center gap-1 text-neo-green">
        <ShieldCheck size={13} /> Signature verified
      </span>
    );
  }
  if (status === 'unverified') {
    return (
      <span className="neo-label-sm flex items-center gap-1 text-neo-red">
        <ShieldAlert size={13} /> Signature invalid
      </span>
    );
  }
  return (
    <span className="neo-label-sm flex items-center gap-1 text-neo-text-muted">
      <ShieldQuestion size={13} /> Checking signature...
    </span>
  );
}

// Renders a manifest readably: the declarative shape (name, entity types,
// views) when it is a full module manifest, falling back to pretty JSON.
function ManifestPreview({ manifest }) {
  if (!manifest || typeof manifest !== 'object') {
    return <span className="neo-label-sm text-neo-text-muted">No manifest.</span>;
  }
  const entityTypes = manifest.entityTypes && typeof manifest.entityTypes === 'object'
    ? Object.entries(manifest.entityTypes)
    : [];
  const views = Array.isArray(manifest.views) ? manifest.views : [];
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-0.5">
        <span className="font-bold text-sm">{manifest.name || manifest.id || 'Module'}</span>
        {manifest.id && <span className="neo-label-sm text-neo-text-muted font-mono">{manifest.id}@{manifest.version || '?'}</span>}
      </div>
      {entityTypes.length > 0 && (
        <div>
          <div className="neo-label-sm text-neo-text-muted mb-1">Entity types</div>
          <div className="flex flex-wrap gap-1.5">
            {entityTypes.map(([key, et]) => (
              <span key={key} className="neo-border px-2 py-0.5 text-xs font-mono bg-neo-bg">
                {et?.label || key}
              </span>
            ))}
          </div>
        </div>
      )}
      {views.length > 0 && (
        <div>
          <div className="neo-label-sm text-neo-text-muted mb-1">Views</div>
          <div className="flex flex-wrap gap-1.5">
            {views.map((v, i) => (
              <span key={v?.id || i} className="neo-border px-2 py-0.5 text-xs font-mono bg-neo-bg">
                {(v?.label || v?.id || 'view')}{v?.kind ? ` · ${v.kind}` : ''}
              </span>
            ))}
          </div>
        </div>
      )}
      <details className="neo-label-sm text-neo-text-muted">
        <summary className="cursor-pointer">Raw manifest</summary>
        <pre className="mt-2 p-2 neo-border bg-neo-bg text-xs font-mono overflow-x-auto max-h-64">
          {JSON.stringify(manifest, null, 2)}
        </pre>
      </details>
    </div>
  );
}

export default function Marketplace() {
  const [packages, setPackages] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [query, setQuery] = useState('');

  // Detail drawer for the selected package.
  const [selected, setSelected] = useState(null);
  const [sigStatus, setSigStatus] = useState('unknown');
  const [versions, setVersions] = useState([]);
  const [versionsLoading, setVersionsLoading] = useState(false);

  // Install progress + honest validation-failure surfacing.
  const [installingId, setInstallingId] = useState(null);
  const [installError, setInstallError] = useState('');
  const [installedId, setInstalledId] = useState(null);

  // Publish-from-installed (gated) flow.
  const [installedModules, setInstalledModules] = useState([]);
  const [publishSourceId, setPublishSourceId] = useState('');
  const [publishVersion, setPublishVersion] = useState('1.0.0');
  const [publishNotice, setPublishNotice] = useState('');
  const [publishing, setPublishing] = useState(false);

  const loadPackages = async () => {
    setLoading(true);
    const { ok, data, error: err } = await apiCall('GET', '/api/marketplace/packages');
    if (ok) setPackages(Array.isArray(data?.packages) ? data.packages : []);
    else setError(err || 'Failed to load marketplace packages.');
    setLoading(false);
  };

  const loadInstalledModules = async () => {
    const { ok, data } = await apiCall('GET', MANIFEST_ENTITY_QUERY);
    setInstalledModules(ok && Array.isArray(data) ? data : []);
  };

  useEffect(() => { loadPackages(); loadInstalledModules(); }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return packages;
    return packages.filter((p) => `${p.module_id} ${p.version}`.toLowerCase().includes(q));
  }, [packages, query]);

  const openDetail = async (pkg) => {
    setSelected(pkg);
    setInstallError('');
    setInstalledId(null);
    setSigStatus('unknown');
    setVersions([]);
    setVersionsLoading(true);

    const verify = apiCall('POST', '/api/marketplace/verify', {
      manifest: pkg.manifest, signature: pkg.signature, pubkey: pkg.publisher_pubkey,
    });
    const list = apiCall('GET', `/api/marketplace/package/${encodeURIComponent(pkg.module_id)}/versions`);
    const [{ ok: vOk, data: vData }, { ok: lOk, data: lData }] = await Promise.all([verify, list]);
    setSigStatus(vOk && vData?.valid ? 'verified' : 'unverified');
    setVersions(lOk && Array.isArray(lData?.versions) ? lData.versions : []);
    setVersionsLoading(false);
  };

  const closeDetail = () => setSelected(null);

  // Install (or roll back to) a specific package version. The server re-runs
  // the validator chain; a failure returns the honest error we surface.
  const install = async (pkg) => {
    setInstallingId(pkg.id);
    setInstallError('');
    setInstalledId(null);
    const { ok, error: err } = await apiCall('POST', '/api/marketplace/install', { package_id: pkg.id });
    setInstallingId(null);
    if (ok) {
      setInstalledId(pkg.id);
      loadInstalledModules();
    } else {
      setInstallError(err || 'Install failed - the package did not pass validation.');
    }
  };

  const requestPublish = async (e) => {
    e.preventDefault();
    setPublishNotice('');
    const source = installedModules.find((m) => (m.attrs?.id || m.title) === publishSourceId);
    const manifest = source?.attrs;
    const moduleId = manifest?.id;
    if (!manifest || !moduleId) {
      setPublishNotice('Pick an installed module to publish.');
      return;
    }
    setPublishing(true);
    // Publishing is outward -> a gated draft in the approval inbox (issue #142),
    // never a direct publish. The execute_approval path signs + stores it.
    const { ok, error: err } = await apiCall('POST', '/api/entity', {
      module: 'bot', type: 'draft', status: 'pending_approval',
      attrs: {
        kind: 'marketplace_publish', module_id: moduleId, version: publishVersion, manifest,
        text: `Publish ${moduleId}@${publishVersion} to the marketplace`,
      },
    });
    setPublishing(false);
    if (ok) setPublishNotice(`Publish request for ${moduleId}@${publishVersion} sent for approval.`);
    else setPublishNotice(err || 'Could not create the publish request.');
  };

  return (
    <div className="p-6 flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Store size={22} />
          <h1 className="neo-title-lg">Module Marketplace</h1>
        </div>
        <button onClick={loadPackages} className="neo-btn px-3 py-2 flex items-center gap-1.5 text-xs font-bold uppercase">
          <RefreshCw size={14} /> Refresh
        </button>
      </div>

      {error && (
        <div className="p-3 bg-neo-red text-white border-2 border-neo-border neo-label-sm flex items-center gap-2">
          <AlertTriangle size={14} /> {error}
        </div>
      )}

      {/* Publish an installed module - human-gated. */}
      <form onSubmit={requestPublish} className="neo-surface neo-border neo-shadow-sm p-4 flex flex-col gap-3">
        <div className="flex items-center gap-2 neo-label-md">
          <UploadCloud size={16} /> Publish an installed module
        </div>
        {installedModules.length === 0 ? (
          <span className="neo-label-sm text-neo-text-muted">No installed modules to publish yet.</span>
        ) : (
          <div className="flex flex-wrap gap-3 items-center">
            <select
              className="p-2 neo-border bg-neo-bg text-xs font-mono flex-1 min-w-40"
              value={publishSourceId}
              onChange={(e) => setPublishSourceId(e.target.value)}
              aria-label="Module to publish"
            >
              <option value="">Select a module...</option>
              {installedModules.map((m) => {
                const id = m.attrs?.id || m.title;
                return <option key={m.id} value={id}>{m.attrs?.name || id}</option>;
              })}
            </select>
            <input
              className="p-2 neo-border bg-neo-bg text-xs font-mono w-32"
              placeholder="version"
              value={publishVersion}
              onChange={(e) => setPublishVersion(e.target.value)}
              aria-label="Publish version"
            />
            <button
              type="submit"
              disabled={publishing || !publishSourceId}
              className="neo-btn px-4 py-2 bg-neo-mint text-black text-xs font-bold uppercase disabled:opacity-50"
            >
              {publishing ? 'Sending...' : 'Request publish'}
            </button>
          </div>
        )}
        {publishNotice && (
          <span className="neo-label-sm flex items-center gap-1.5 text-neo-text">
            <CheckCircle2 size={13} /> {publishNotice}
          </span>
        )}
      </form>

      {/* Search + browse. */}
      <div className="flex items-center gap-2 neo-border bg-neo-bg px-3">
        <Search size={15} className="text-neo-text-muted" />
        <input
          className="p-2 bg-transparent text-xs font-mono flex-1 outline-none"
          placeholder="Search packages..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="Search packages"
        />
      </div>

      <div className="flex flex-col gap-3">
        {loading && <span className="neo-label-sm text-neo-text-muted">Loading packages...</span>}
        {!loading && filtered.length === 0 && (
          <span className="neo-label-sm text-neo-text-muted">
            {query ? 'No packages match your search.' : 'No packages published yet.'}
          </span>
        )}
        {filtered.map((pkg) => (
          <button
            key={pkg.id}
            onClick={() => openDetail(pkg)}
            className="neo-surface neo-border neo-shadow-sm p-4 flex items-center justify-between text-left hover:bg-neo-bg"
          >
            <div className="flex flex-col gap-1">
              <span className="font-bold text-sm">{pkg.module_id}@{pkg.version}</span>
              <span className="neo-label-sm text-neo-text-muted flex items-center gap-1 font-mono">
                <ShieldCheck size={12} /> {shortKey(pkg.publisher_pubkey)}
              </span>
            </div>
            <span className="neo-label-sm text-neo-text-muted">Details &rarr;</span>
          </button>
        ))}
      </div>

      {selected && (
        <Drawer
          pkg={selected}
          sigStatus={sigStatus}
          versions={versions}
          versionsLoading={versionsLoading}
          installingId={installingId}
          installError={installError}
          installedId={installedId}
          onInstall={install}
          onClose={closeDetail}
        />
      )}
    </div>
  );
}

// The detail drawer: manifest preview, signature badge, version history with
// per-version install (rollback), and the primary install with progress +
// validation-failure display.
function Drawer({ pkg, sigStatus, versions, versionsLoading, installingId, installError, installedId, onInstall, onClose }) {
  return (
    <div className="fixed inset-0 z-40 flex justify-end" role="dialog" aria-label="Package details">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative z-50 w-full max-w-md h-full bg-neo-surface border-l-2 border-neo-border overflow-y-auto p-5 flex flex-col gap-5">
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <span className="font-bold text-base">{pkg.module_id}@{pkg.version}</span>
            <SignatureBadge status={sigStatus} />
          </div>
          <button onClick={onClose} className="neo-btn p-2" aria-label="Close details"><X size={16} /></button>
        </div>

        <ManifestPreview manifest={pkg.manifest} />

        <div className="flex flex-col gap-2">
          <button
            onClick={() => onInstall(pkg)}
            disabled={installingId === pkg.id}
            className="neo-btn px-4 py-2 flex items-center justify-center gap-1.5 text-xs font-bold uppercase bg-neo-yellow text-black disabled:opacity-50"
          >
            <DownloadCloud size={14} /> {installingId === pkg.id ? 'Validating & installing...' : 'Install'}
          </button>
          {installedId === pkg.id && (
            <span className="neo-label-sm flex items-center gap-1.5 text-neo-green">
              <CheckCircle2 size={13} /> Installed - validated and activated.
            </span>
          )}
          {installError && (
            <div className="p-2 bg-neo-red text-white neo-border neo-label-sm flex items-start gap-1.5">
              <AlertTriangle size={13} className="mt-0.5 shrink-0" /> <span>{installError}</span>
            </div>
          )}
        </div>

        <div className="flex flex-col gap-2">
          <div className="neo-label-md flex items-center gap-1.5"><History size={14} /> Version history</div>
          {versionsLoading && <span className="neo-label-sm text-neo-text-muted">Loading versions...</span>}
          {!versionsLoading && versions.length === 0 && (
            <span className="neo-label-sm text-neo-text-muted">No version history.</span>
          )}
          {versions.map((v) => (
            <div key={v.id} className="neo-border p-2 flex items-center justify-between bg-neo-bg">
              <span className="text-xs font-mono">{v.version}</span>
              <button
                onClick={() => onInstall(v)}
                disabled={installingId === v.id}
                className="neo-btn px-2 py-1 text-xs font-bold uppercase"
              >
                {installingId === v.id ? 'Installing...' : (v.id === pkg.id ? 'Reinstall' : 'Roll back')}
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
