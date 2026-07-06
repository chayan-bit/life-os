// Issue #122 follow-up: the AI Console's primary path is now the backend
// plan -> execute -> verify loop (POST /api/agent), not bespoke /api/llm
// prompt-and-parse. Local browser navigation stays client-side; the guardrail
// pre-filter still short-circuits obviously-forbidden asks before the network.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import AIConsole from './AIConsole';

const navigateMock = vi.fn();

vi.mock('react-router-dom', () => ({ useNavigate: () => navigateMock }));
vi.mock('./MarkdownRenderer', () => ({ default: ({ content }) => <div data-md>{content}</div> }));
vi.mock('../lib/api', () => ({ apiCall: vi.fn(), WORKSPACE_ID_KEY: 'life_os_workspace_id' }));

import { apiCall } from '../lib/api';

async function openAndSubmit(text) {
  fireEvent.click(screen.getByTitle('Ask AI to change anything'));
  const box = screen.getByPlaceholderText(/Change anything/);
  fireEvent.change(box, { target: { value: text } });
  fireEvent.click(screen.getByRole('button', { name: /Send to AI/ }));
}

afterEach(() => { cleanup(); navigateMock.mockReset(); });

describe('AIConsole', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: {}, error: null });
  });

  it('routes the prompt to POST /api/agent and renders the loop final text', async () => {
    apiCall.mockResolvedValueOnce({
      ok: true,
      data: { success: true, outcome: 'completed', text: 'Tagged 3 tasks urgent.', ledger: [] },
      error: null,
    });

    render(<AIConsole />);
    await openAndSubmit('Find my overdue tasks and tag them urgent');

    await waitFor(() => expect(screen.getByText('Tagged 3 tasks urgent.')).toBeTruthy());
    const call = apiCall.mock.calls.find((c) => c[1] === '/api/agent');
    expect(call).toBeTruthy();
    expect(call[0]).toBe('POST');
    expect(call[2].prompt).toBe('Find my overdue tasks and tag them urgent');
  });

  it('renders executed steps and a pending-approval notice for gated drafts', async () => {
    apiCall.mockResolvedValueOnce({
      ok: true,
      data: {
        success: true,
        outcome: 'awaiting_approval',
        text: 'Drafted a summary for you.',
        ledger: [{ tool: 'entity.update', decision: 'applied', ok: true }],
        pendingApprovals: [{ tool: 'draft.create', entityId: 'ent_1' }],
      },
      error: null,
    });

    render(<AIConsole />);
    await openAndSubmit('Draft a summary of this week and post it');

    await waitFor(() => expect(screen.getByText(/Awaiting approval/)).toBeTruthy());
    expect(screen.getByText(/draft\.create/)).toBeTruthy();
    expect(screen.getByText(/1 step/)).toBeTruthy();
  });

  it('handles a timeout / error response gracefully', async () => {
    apiCall.mockResolvedValueOnce({ ok: false, data: null, error: 'agent turn timed out' });

    render(<AIConsole />);
    await openAndSubmit('Do something enormous');

    await waitFor(() => expect(screen.getByText(/couldn't run/)).toBeTruthy());
    expect(screen.getByText(/time limit/)).toBeTruthy();
  });

  it('keeps local navigation client-side and never hits /api/agent', async () => {
    render(<AIConsole />);
    await openAndSubmit('go to the dashboard');

    await waitFor(() => expect(navigateMock).toHaveBeenCalledWith('/dashboard'));
    expect(apiCall.mock.calls.some((c) => c[1] === '/api/agent')).toBe(false);
  });

  it('short-circuits a gated-layer request with the guardrail pre-filter', async () => {
    render(<AIConsole />);
    await openAndSubmit('Delete the version history');

    await waitFor(() => expect(screen.getByText('Guardrail')).toBeTruthy());
    expect(apiCall.mock.calls.some((c) => c[1] === '/api/agent')).toBe(false);
  });
});

// Finding 56: the close ('X') button had no accessible name, Escape didn't
// close the panel, and focus wasn't managed on open/close.
describe('AIConsole accessibility', () => {
  beforeEach(() => {
    apiCall.mockReset();
    apiCall.mockResolvedValue({ ok: true, data: {}, error: null });
  });

  it('gives the close button an accessible name', () => {
    render(<AIConsole />);
    fireEvent.click(screen.getByTitle('Ask AI to change anything'));
    expect(screen.getByRole('button', { name: 'Close AI console' })).toBeTruthy();
  });

  it('focuses the composer on open', () => {
    render(<AIConsole />);
    fireEvent.click(screen.getByTitle('Ask AI to change anything'));
    expect(document.activeElement).toBe(screen.getByPlaceholderText(/Change anything/));
  });

  it('closes on Escape and returns focus to the launcher', async () => {
    render(<AIConsole />);
    const launcher = screen.getByRole('button', { name: 'Open AI console' });
    fireEvent.click(launcher);
    expect(screen.getByRole('button', { name: 'Close AI console' })).toBeTruthy();

    fireEvent.keyDown(window, { key: 'Escape' });

    await waitFor(() => expect(screen.queryByRole('button', { name: 'Close AI console' })).toBeNull());
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Open AI console' }));
  });
});
