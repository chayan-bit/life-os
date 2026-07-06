import React, { useEffect, useState } from 'react';
import { ImageOff } from 'lucide-react';
import { resolveDisplay, resolveField } from './displayHelpers';
import { fetchBlob } from '../../lib/vcsApi';

// Generic gallery renderer: media entities (Design assets, ingest segments,
// ...) as a thumbnail grid driven by manifest display config. A blob_ref is
// content-addressed, not a URL - it's resolved through GET /api/vcs/blob
// (frontend/src/lib/vcsApi.js::fetchBlob, live since issue #109, same route
// BlobView.jsx renders raw bytes with) into an object URL. Direct http(s)
// URLs (an already-hosted asset) pass through unchanged. Anything that
// fails to resolve renders an honest placeholder instead of a broken <img>.
const isHttpUrl = (ref) => /^https?:\/\//.test(ref || '');

function GalleryImage({ blobRef, mime, title }) {
  const [objectUrl, setObjectUrl] = useState(null);

  useEffect(() => {
    setObjectUrl(null);
    if (!blobRef || isHttpUrl(blobRef)) return undefined;

    let cancelled = false;
    let createdUrl = null;
    fetchBlob(blobRef)
      .then((bytes) => {
        if (cancelled) return;
        createdUrl = URL.createObjectURL(new Blob([bytes], { type: mime || 'application/octet-stream' }));
        setObjectUrl(createdUrl);
      })
      .catch(() => {}); // resolveSrc below falls back to the placeholder

    return () => {
      cancelled = true;
      if (createdUrl) URL.revokeObjectURL(createdUrl);
    };
  }, [blobRef, mime]);

  const src = isHttpUrl(blobRef) ? blobRef : objectUrl;

  return src ? (
    <img src={src} alt={title} className="w-full h-full object-cover" />
  ) : (
    <ImageOff size={28} className="text-neo-text-muted" />
  );
}

export default function GenericGallery({ entities, display = {}, mediaField = 'blob_ref', emptyLabel = 'No media yet.' }) {
  if (!entities?.length) {
    return <p className="text-xs text-neo-text-muted">{emptyLabel}</p>;
  }

  return (
    <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
      {entities.map((entity) => {
        const { title, badge } = resolveDisplay(entity, display);
        const blobRef = resolveField(entity, mediaField);
        const mime = resolveField(entity, 'mime');
        return (
          <div key={entity.id} className="neo-border bg-neo-bg flex flex-col overflow-hidden">
            <div className="aspect-square bg-neo-surface-muted flex items-center justify-center">
              <GalleryImage blobRef={blobRef} mime={mime} title={title} />
            </div>
            <div className="p-2 flex flex-col gap-1">
              <span className="text-xs font-bold truncate">{title}</span>
              {badge && <span className="neo-tag text-[8px] font-mono self-start">{badge}</span>}
            </div>
          </div>
        );
      })}
    </div>
  );
}
