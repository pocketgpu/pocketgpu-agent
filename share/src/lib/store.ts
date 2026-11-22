// What this browser keeps for each link it opened (F48), in IndexedDB on share.pocketgpu.app
// only: its own key for the computer, the link itself (so a reload works after the address
// bar is cleaned), and the chat. Nothing of it goes anywhere but the computer.

export type Role = 'User' | 'Assistant';
export type Message = {
	role: Role;
	text: string;
	at: number;
	/** The answer's speed, when the computer sent it. */
	speed?: { tokens_per_s: number; ms: number };
};
export type Saved = { id: string; fragment: string; key: number[]; claimed: boolean };

const DB = 'pocketgpu-share';
const LINKS = 'links';
const CHATS = 'chats';
/** As the computer allows: 40 messages and 64 KB with each chat (proto share.rs). */
export const MAX_MESSAGES = 40;
export const MAX_TOTAL_BYTES = 64 * 1024;
export const MAX_MESSAGE_BYTES = 8 * 1024;

function open(): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const req = indexedDB.open(DB, 1);
		req.onupgradeneeded = () => {
			const db = req.result;
			if (!db.objectStoreNames.contains(LINKS))
				db.createObjectStore(LINKS, { keyPath: 'id' });
			if (!db.objectStoreNames.contains(CHATS)) db.createObjectStore(CHATS);
		};
		req.onsuccess = () => resolve(req.result);
		req.onerror = () => reject(req.error);
	});
}

async function run<T>(
	store: string,
	mode: IDBTransactionMode,
	f: (s: IDBObjectStore) => IDBRequest<T>
): Promise<T> {
	const db = await open();
	return new Promise((resolve, reject) => {
		const req = f(db.transaction(store, mode).objectStore(store));
		req.onsuccess = () => resolve(req.result);
		req.onerror = () => reject(req.error);
	});
}

export const getLink = (id: string) =>
	run<Saved | undefined>(LINKS, 'readonly', (s) => s.get(id) as IDBRequest<Saved | undefined>);
export const putLink = (link: Saved) => run(LINKS, 'readwrite', (s) => s.put(link));
/** The link opened last, for a reload after the address bar was cleaned. */
export async function lastLink(): Promise<Saved | undefined> {
	const all = await run<Saved[]>(LINKS, 'readonly', (s) => s.getAll() as IDBRequest<Saved[]>);
	return all.at(-1);
}

export const getChat = async (id: string) =>
	(await run<Message[] | undefined>(CHATS, 'readonly', (s) => s.get(id))) ?? [];
export const putChat = (id: string, messages: Message[]) =>
	run(CHATS, 'readwrite', (s) => s.put(messages, id));

/** The newest messages that fit what the computer accepts, oldest first, ending with the
 *  guest's question. A message too long on its own is cut to fit. */
export function context(messages: Message[]): { role: Role; text: string }[] {
	const out: { role: Role; text: string }[] = [];
	let bytes = 0;
	const size = (t: string) => new TextEncoder().encode(t).length;
	for (let i = messages.length - 1; i >= 0 && out.length < MAX_MESSAGES; i--) {
		let text = messages[i].text;
		while (size(text) > MAX_MESSAGE_BYTES) text = text.slice(0, Math.floor(text.length * 0.9));
		if (bytes + size(text) > MAX_TOTAL_BYTES) break;
		bytes += size(text);
		out.unshift({ role: messages[i].role, text });
	}
	while (out.length && out[0].role === 'Assistant') out.shift();
	return out;
}
