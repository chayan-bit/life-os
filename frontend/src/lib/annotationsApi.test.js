// The atlas UI shape and the /api/annotation row shape are deliberately
// different (the schema has no column for link/answer/createdAt - they ride in
// attrs). fromApi/toApi are the only bridge, so a round-trip must preserve
// every field the notes panel renders, or notes silently lose their quote,
// link, or AI answer after a reload.
import { describe, expect, it } from 'vitest';
import { fromApi, toApi } from './annotationsApi';

describe('annotationsApi mapping', () => {
  it('round-trips a UI annotation through toApi -> fromApi', () => {
    const ui = {
      id: 'ann_1',
      kind: 'question',
      topicId: 'topic-sockets',
      anchorType: 'selection',
      quote: 'BSD sockets',
      text: 'why non-blocking?',
      link: null,
      createdAt: '2026-07-06T00:00:00.000Z',
      answer: 'because epoll scales',
      answeredAt: '2026-07-06T00:01:00.000Z',
    };
    // Simulate the server echoing back the create payload as a stored row.
    const payload = toApi(ui);
    const row = {
      id: 'ann_1',
      workspace_id: 'ws',
      entity_id: payload.entity_id,
      kind: payload.kind,
      body: payload.body,
      anchor: payload.anchor,
      attrs: payload.attrs,
      created_by: 'user',
      created_at: 1751760000,
      updated_at: 1751760000,
    };
    const back = fromApi(row);
    expect(back.kind).toBe('question');
    expect(back.topicId).toBe('topic-sockets');
    expect(back.quote).toBe('BSD sockets');
    expect(back.text).toBe('why non-blocking?');
    expect(back.answer).toBe('because epoll scales');
    expect(back.answeredAt).toBe('2026-07-06T00:01:00.000Z');
    expect(back.createdAt).toBe('2026-07-06T00:00:00.000Z');
  });

  it('parses a string anchor column and preserves a link payload', () => {
    const row = {
      id: 'ann_2',
      kind: 'link',
      entity_id: 'topic-b',
      body: '',
      anchor: JSON.stringify({ type: 'resource', quote: 'see also' }),
      attrs: { link: { to: 'topic-c', url: 'https://x.test', note: 'related' } },
      created_at: 1751760000,
    };
    const ui = fromApi(row);
    expect(ui.anchorType).toBe('resource');
    expect(ui.quote).toBe('see also');
    expect(ui.link.to).toBe('topic-c');
    expect(ui.link.url).toBe('https://x.test');
  });

  it('falls back to created_at seconds when attrs has no createdAt', () => {
    const ui = fromApi({ id: 'ann_3', kind: 'note', body: 'x', attrs: {}, created_at: 1751760000 });
    expect(ui.createdAt).toBe(new Date(1751760000 * 1000).toISOString());
    expect(ui.answer).toBe('');
    expect(ui.answeredAt).toBeNull();
  });
});
