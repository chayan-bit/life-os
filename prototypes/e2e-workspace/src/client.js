// The E2E client: the ONLY party that ever holds plaintext or the content key.
//
// This is the trusted device (the Mac harness, the PWA on your phone, or a
// lifeos-node you run). It mirrors what a real Life OS client would do around
// the existing lifeos-api HTTP surface, but with encryption pushed to the edge:
//
//   put():   encrypt attrs/title client-side, send only ciphertext to server
//   get():   fetch ciphertext, decrypt client-side
//   openSession(): re-derive the member KEK from the passphrase, unwrap the
//                  per-workspace content key from the server-stored wrapped key
//   buildLocalIndex(): fetch+decrypt EVERYTHING, build a local BM25 index
//   localSearch(): query that index in-process (no server involvement)
//
// The content key (CWK) lives only in this object's memory for the session.

import {
  deriveMemberKey,
  randomKey,
  randomSalt,
  wrapKey,
  unwrapKey,
  seal,
  open,
} from "./crypto.js";
import { buildIndex, search, estimateIndexBytes } from "./localIndex.js";

export class E2EClient {
  constructor(server, workspaceId, memberId) {
    this._server = server;
    this._workspaceId = workspaceId;
    this._memberId = memberId;
    this._cwk = null; // content key, memory-only, never sent to the server
    this._index = null;
  }

  /**
   * First-ever member of a fresh E2E workspace: mint the content key, wrap it
   * under a KEK derived from this member's passphrase, and publish only the
   * salt + wrapped key. The server never sees the CWK or the passphrase.
   */
  bootstrapWorkspace(passphrase) {
    const salt = randomSalt();
    const kek = deriveMemberKey(passphrase, salt);
    const cwk = randomKey();
    const wrapped = wrapKey(cwk, kek);
    this._server.putMemberKey(
      this._workspaceId,
      this._memberId,
      salt.toString("base64"),
      wrapped,
    );
    this._cwk = cwk;
    return cwk;
  }

  /** Re-open the workspace on a fresh device using only the passphrase. */
  openSession(passphrase) {
    const saltB64 = this._server.getMemberSalt(this._workspaceId, this._memberId);
    const wrapped = this._server.getWrappedKey(this._workspaceId, this._memberId);
    const kek = deriveMemberKey(passphrase, Buffer.from(saltB64, "base64"));
    this._cwk = unwrapKey(wrapped, kek); // throws (fail closed) on wrong passphrase
    return this._cwk;
  }

  /**
   * Add another member: rewrap the SAME content key under the new member's
   * KEK. Requires an already-authorized member online (this one) - the server
   * cannot do it because it never holds the CWK. This is the membership-add
   * cost the threat model calls out.
   */
  addMember(newMemberId, newMemberPassphrase) {
    if (this._cwk === null) throw new Error("must open a session before adding members");
    const salt = randomSalt();
    const kek = deriveMemberKey(newMemberPassphrase, salt);
    const wrapped = wrapKey(this._cwk, kek);
    this._server.putMemberKey(
      this._workspaceId,
      newMemberId,
      salt.toString("base64"),
      wrapped,
    );
  }

  /** Encrypt an entity client-side and store only ciphertext on the server. */
  put(entity) {
    if (this._cwk === null) throw new Error("no content key: open a session first");
    const attrsJson = JSON.stringify(entity.attrs ?? {});
    this._server.storeRow(this._workspaceId, {
      id: entity.id,
      module: entity.module, // structural metadata stays plaintext (leaks)
      type: entity.type,
      createdAt: entity.createdAt,
      title: entity.title != null ? seal(entity.title, this._cwk) : null,
      attrs: seal(attrsJson, this._cwk), // the content the server must never read
    });
  }

  /** Fetch + decrypt one entity client-side. */
  get(entityId) {
    const row = this._server.fetchRow(this._workspaceId, entityId);
    if (row === null) return null;
    return {
      id: row.id,
      module: row.module,
      type: row.type,
      createdAt: row.createdAt,
      title: row.title != null ? open(row.title, this._cwk).toString("utf8") : null,
      attrs: JSON.parse(open(row.attrs, this._cwk).toString("utf8")),
    };
  }

  /**
   * Build the LOCAL search index: fetch every ciphertext row, decrypt it, and
   * index the plaintext on-device. This is the O(N) bootstrap tax that E2E
   * pays in place of free server-side FTS. Returns measured stats.
   */
  buildLocalIndex() {
    const rows = this._server.dumpRows(this._workspaceId);
    let bytesFetched = 0;
    const docs = rows.map((row) => {
      bytesFetched += (row.attrs?.length ?? 0) + (row.title?.length ?? 0);
      const attrs = JSON.parse(open(row.attrs, this._cwk).toString("utf8"));
      const title = row.title != null ? open(row.title, this._cwk).toString("utf8") : "";
      const text = [title, ...Object.values(attrs).filter((v) => typeof v === "string")].join(" ");
      return { id: row.id, text };
    });
    this._index = buildIndex(docs);
    return {
      docs: docs.length,
      bytesFetched,
      indexBytes: estimateIndexBytes(this._index),
    };
  }

  /** Query the local index. No server round-trip; runs over decrypted data. */
  localSearch(query, limit = 10) {
    if (this._index === null) throw new Error("call buildLocalIndex() first");
    return search(this._index, query, limit);
  }
}
