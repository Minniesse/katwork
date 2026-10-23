/**
 * A fresh UUID v4 for a post's `client_msg_id`. Browsers expose `crypto.randomUUID` only in
 * secure contexts (HTTPS or localhost), and a LAN install is opened over plain HTTP, so the id
 * is built from `crypto.getRandomValues`, which has no such restriction, when it is missing.
 */
export function newClientMsgId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 0x0f) | 0x40; // version 4
  bytes[8] = (bytes[8]! & 0x3f) | 0x80; // RFC 4122 variant
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
