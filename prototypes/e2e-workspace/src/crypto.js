// Client-side envelope crypto for the E2E-workspace spike.
//
// Wire format is intentionally identical to the shipped Rust machinery so a
// real build can share blobs byte-for-byte:
//   - services/lifeos-api/src/crypto.rs      (secret_enc: base64(nonce || ct))
//   - services/lifeos-vcs/src/encrypted.rs   (blobs:      nonce || ct)
// where `ct` is AES-256-GCM ciphertext with its 16-byte auth tag appended
// (the `aes-gcm` crate returns ct||tag; Node returns them separately, so we
// concatenate to match). Every blob is `base64(nonce(12) || ct || tag(16))`.
//
// The ONE difference that makes this end-to-end rather than server-custodied:
// the content key is never wrapped under a server-held master key. It is
// wrapped under a MEMBER key derived from a passphrase (Argon2id in prod; here
// scrypt, a Node built-in with no external dependency). The server therefore
// only ever stores wrapped keys and ciphertext.

import {
  randomBytes,
  createCipheriv,
  createDecipheriv,
  scryptSync,
} from "node:crypto";

export const NONCE_LEN = 12;
export const TAG_LEN = 16;
export const KEY_LEN = 32;

// scrypt cost parameters. N=2^15 is a deliberate spike default: strong enough
// to be a realistic KDF, cheap enough that the demo runs in well under a
// second. Production would pin Argon2id (memory-hard, side-channel resistant)
// or a WebAuthn passkey PRF; the shape (passphrase -> KEK) is unchanged.
const SCRYPT_PARAMS = Object.freeze({ N: 1 << 15, r: 8, p: 1, maxmem: 64 << 20 });

/** Fresh random 32-byte key (a content key, or any raw AES-256 key). */
export function randomKey() {
  return randomBytes(KEY_LEN);
}

/**
 * Derive a member Key-Encrypting-Key (KEK) from a passphrase + per-member salt.
 * The KEK never leaves the client and is never stored; only the salt is stored
 * server-side (public, per DATA-MODEL: salts are not secrets).
 */
export function deriveMemberKey(passphrase, salt) {
  return scryptSync(passphrase, salt, KEY_LEN, SCRYPT_PARAMS);
}

/**
 * Encrypt `plaintext` (string or Buffer) under `key`, returning a base64 blob
 * of `nonce || ct || tag`. Random nonce per call => two encryptions of the
 * same plaintext are unlinkable, exactly like crypto.rs.
 */
export function seal(plaintext, key) {
  const nonce = randomBytes(NONCE_LEN);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  const data = Buffer.isBuffer(plaintext) ? plaintext : Buffer.from(plaintext, "utf8");
  const ct = Buffer.concat([cipher.update(data), cipher.final()]);
  const tag = cipher.getAuthTag();
  return Buffer.concat([nonce, ct, tag]).toString("base64");
}

/**
 * Reverse `seal`. Fails closed (throws) on any tamper / wrong key / short blob
 * rather than returning partial plaintext, matching the Rust "fail closed"
 * contract. Returns a Buffer; callers that stored UTF-8 call `.toString()`.
 */
export function open(blob, key) {
  const raw = Buffer.from(blob, "base64");
  if (raw.length < NONCE_LEN + TAG_LEN) {
    throw new Error("sealed blob too short");
  }
  const nonce = raw.subarray(0, NONCE_LEN);
  const tag = raw.subarray(raw.length - TAG_LEN);
  const ct = raw.subarray(NONCE_LEN, raw.length - TAG_LEN);
  const decipher = createDecipheriv("aes-256-gcm", key, nonce);
  decipher.setAuthTag(tag);
  return Buffer.concat([decipher.update(ct), decipher.final()]);
}

/** Wrap a content key under a member KEK (produces the server-stored blob). */
export function wrapKey(contentKey, memberKek) {
  return seal(contentKey, memberKek);
}

/** Unwrap a content key using the member KEK. Throws if the KEK is wrong. */
export function unwrapKey(wrapped, memberKek) {
  const key = open(wrapped, memberKek);
  if (key.length !== KEY_LEN) throw new Error("unwrapped key is not 32 bytes");
  return key;
}

export function randomSalt() {
  return randomBytes(16);
}
