// The link's fragment (everything after #), F48. It never leaves this page: it isn't sent to
// any server, and the address bar drops it once it is read, so a screenshot or a copied
// address doesn't carry the secret. The wasm reads the fragment again in full; this only
// checks it looks right and names the place it is kept under.

const MAX = 600;
const B64URL = /^[A-Za-z0-9_-]+$/;

export type Fragment = { raw: string; id: string };

/** The fragment from an address's hash, or null when there is none or it can't be one. */
export function readFragment(hash: string): Fragment | null {
	const raw = hash.replace(/^#/, '').trim();
	if (!raw || raw.length > MAX || !B64URL.test(raw)) return null;
	return { raw, id: storageId(raw) };
}

/** A short, stable name for this link in the browser's storage (not the secret itself). */
export function storageId(raw: string): string {
	let h = 0x811c9dc5;
	for (let i = 0; i < raw.length; i++) {
		h ^= raw.charCodeAt(i);
		h = Math.imul(h, 0x01000193) >>> 0;
	}
	return `l${h.toString(36)}${raw.length.toString(36)}`;
}
