// Local user-identity helpers (issue #54 + #150). The app persists the signed-in
// user's email under `life_os_user_email` (LoginPage), but nothing ever wrote
// `life_os_user_name` - so the header avatar's old `life_os_user_name || 'LO'`
// read always fell through to the literal 'LO'. These helpers derive the avatar
// from whatever real identity we actually have, and clear it on logout (the
// email key used to survive a logout, leaking the previous user's address into
// the next session's Profile view).

export const USER_EMAIL_KEY = 'life_os_user_email';
export const USER_NAME_KEY = 'life_os_user_name';

/// Two-letter avatar initials from the real identity: a full name if present,
/// otherwise the email's local part, otherwise the 'LO' brand fallback.
export function avatarInitials() {
  const name = (localStorage.getItem(USER_NAME_KEY) || '').trim();
  if (name) {
    const initials = name
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0])
      .join('');
    if (initials) return initials.toUpperCase();
  }

  const email = (localStorage.getItem(USER_EMAIL_KEY) || '').trim();
  if (email) {
    const local = email.split('@')[0] || email;
    const tokens = local.split(/[._+-]+/).filter(Boolean);
    const initials =
      tokens.length >= 2
        ? tokens
            .slice(0, 2)
            .map((t) => t[0])
            .join('')
        : local.slice(0, 2);
    if (initials) return initials.toUpperCase();
  }

  return 'LO';
}

/// Remove every local user-identity key. Called from the logout handler so a
/// signed-out session leaves no trace of the previous user's identity behind.
export function clearIdentity() {
  localStorage.removeItem(USER_EMAIL_KEY);
  localStorage.removeItem(USER_NAME_KEY);
}
