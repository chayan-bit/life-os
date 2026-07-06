// Issue #54: the header avatar used to read `life_os_user_name` (never written)
// and always showed 'LO', and the email key survived logout. These cover the
// fixed initials derivation and the logout-time identity clear.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { avatarInitials, clearIdentity, USER_EMAIL_KEY, USER_NAME_KEY } from './identity';

describe('avatarInitials', () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => localStorage.clear());

  it("falls back to 'LO' when no identity is stored", () => {
    expect(avatarInitials()).toBe('LO');
  });

  it('derives two initials from a dotted email local part (the real #54 case)', () => {
    localStorage.setItem(USER_EMAIL_KEY, 'chayan.aggarwal@example.com');
    expect(avatarInitials()).toBe('CA');
  });

  it('uses the first two letters when the email local part is a single token', () => {
    localStorage.setItem(USER_EMAIL_KEY, 'chayan@example.com');
    expect(avatarInitials()).toBe('CH');
  });

  it('prefers a full name over the email when one exists', () => {
    localStorage.setItem(USER_NAME_KEY, 'Chayan Aggarwal');
    localStorage.setItem(USER_EMAIL_KEY, 'x@y.com');
    expect(avatarInitials()).toBe('CA');
  });
});

describe('clearIdentity', () => {
  it('removes the email and name keys so logout leaks no identity', () => {
    localStorage.setItem(USER_EMAIL_KEY, 'prev@user.com');
    localStorage.setItem(USER_NAME_KEY, 'Prev User');
    clearIdentity();
    expect(localStorage.getItem(USER_EMAIL_KEY)).toBeNull();
    expect(localStorage.getItem(USER_NAME_KEY)).toBeNull();
  });
});
