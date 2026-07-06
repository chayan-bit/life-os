// Generates one VAPID (RFC 8292) keypair for Web Push (issue #151). Prints
// the three env vars `lifeos-drain`'s `push` module and `lifeos-api`'s
// `GET /api/push/vapid-public-key` route read directly.
//
// Deliberately no `npx web-push generate-vapid-keys` (an ad-hoc package
// fetch) and no `openssl` shell-out: Node's built-in `crypto.createECDH`
// already speaks the raw P-256 point/scalar format VAPID needs, so this needs
// no new dependency - `server/` already has npm/node, nothing else to install.
//
// Usage: node server/scripts/generateVapidKeys.js
import crypto from "node:crypto";

const P256_COORDINATE_BYTES = 32; // P-256 (prime256v1) private scalar length

function base64UrlNoPad(buffer) {
  return buffer.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// `ecdh.getPrivateKey()` does not zero-pad: a private scalar with a leading
// zero byte comes back shorter than 32 bytes, which would silently produce an
// invalid VAPID key. Left-pad to the curve's fixed scalar length.
function zeroPadLeft(buffer, length) {
  return buffer.length >= length ? buffer : Buffer.concat([Buffer.alloc(length - buffer.length), buffer]);
}

// Pure keypair generator - exported so it's unit-testable without shelling
// out or touching stdout.
export function generateVapidKeys() {
  const ecdh = crypto.createECDH("prime256v1");
  ecdh.generateKeys();
  const publicKey = ecdh.getPublicKey(); // uncompressed point: 0x04 || X || Y, 65 bytes
  const privateKey = zeroPadLeft(ecdh.getPrivateKey(), P256_COORDINATE_BYTES);
  return {
    publicKey: base64UrlNoPad(publicKey),
    privateKey: base64UrlNoPad(privateKey),
  };
}

function main() {
  const { publicKey, privateKey } = generateVapidKeys();
  console.log(`LIFEOS_VAPID_PUBLIC_KEY=${publicKey}`);
  console.log(`LIFEOS_VAPID_PRIVATE_KEY=${privateKey}`);
  console.log("LIFEOS_VAPID_SUBJECT=mailto:you@example.com");
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
