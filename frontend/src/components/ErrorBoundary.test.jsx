// Finding 22: a reusable render-error boundary so a thrown error inside a
// page's render tree shows a friendly fallback (+ retry) instead of a blank
// white screen.
import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ErrorBoundary from './ErrorBoundary';

afterEach(cleanup);

// React logs a loud "error occurred in ... component" message to console.error
// whenever a boundary catches - expected noise for these tests, quiet it down.
let consoleErrorSpy;
beforeEach(() => {
  consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
});

describe('ErrorBoundary', () => {
  it('renders children normally when nothing throws', () => {
    render(
      <ErrorBoundary>
        <div>All good</div>
      </ErrorBoundary>
    );
    expect(screen.getByText('All good')).toBeTruthy();
  });

  it('renders a friendly fallback when a child throws during render', () => {
    function Bomb() {
      throw new Error('kaboom');
    }
    render(
      <ErrorBoundary>
        <Bomb />
      </ErrorBoundary>
    );
    expect(screen.getByText(/Something went wrong/i)).toBeTruthy();
    expect(screen.getByText('kaboom')).toBeTruthy();
    expect(consoleErrorSpy).toHaveBeenCalled();
  });

  it('recovers and re-renders children when retry is clicked and the error condition has cleared', async () => {
    let shouldThrow = true;
    function Bomb() {
      if (shouldThrow) throw new Error('kaboom');
      return <div>Recovered</div>;
    }

    render(
      <ErrorBoundary>
        <Bomb />
      </ErrorBoundary>
    );
    expect(screen.getByText(/Something went wrong/i)).toBeTruthy();

    shouldThrow = false;
    fireEvent.click(screen.getByRole('button', { name: /Retry/i }));

    await waitFor(() => expect(screen.getByText('Recovered')).toBeTruthy());
  });
});
