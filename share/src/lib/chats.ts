// The guest's chats with one link (board "Share a model", slots "New chat and earlier chats"
// and "Your chats"): kept in this browser only, the open one first to come back to. The
// computer only ever sees the chat being sent.
import type { Message } from './store.js';

export type Chat = { id: string; messages: Message[]; at: number };
export type Chats = { open: string; chats: Chat[] };

/** Older chats beyond this are let go, so the browser never fills up. */
export const MAX_CHATS = 30;

export const newId = () =>
	typeof crypto.randomUUID === 'function'
		? crypto.randomUUID()
		: `${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;

/** What was saved for a link: pages from before several chats kept one list of messages. */
export function fromSaved(v: unknown): Chats {
	if (Array.isArray(v)) {
		const messages = v as Message[];
		const id = newId();
		const at = messages.at(-1)?.at ?? Date.now();
		return { open: id, chats: messages.length ? [{ id, messages, at }] : [] };
	}
	const c = v as Partial<Chats> | undefined;
	if (c && typeof c.open === 'string' && Array.isArray(c.chats))
		return { open: c.open, chats: c.chats };
	return { open: newId(), chats: [] };
}

/** The open chat's messages. */
export const openMessages = (all: Chats): Message[] =>
	all.chats.find((c) => c.id === all.open)?.messages ?? [];

/** The open chat with these messages: a chat with nothing in it isn't listed. Only the
 *  newest `MAX_CHATS` are kept, never letting go of the open one. */
export function withMessages(all: Chats, messages: Message[], now = Date.now()): Chats {
	const rest = all.chats.filter((c) => c.id !== all.open);
	const open = messages.length ? [{ id: all.open, messages, at: now }] : [];
	const kept = newest(rest).slice(0, MAX_CHATS - open.length);
	return { open: all.open, chats: [...open, ...kept] };
}

/** A new, empty chat; the one before stays in the list. */
export const fresh = (all: Chats): Chats => ({ open: newId(), chats: all.chats });

/** Newest first. */
export const newest = (chats: Chat[]) => [...chats].sort((a, b) => b.at - a.at);

/** A chat's name: its first question on one line. */
export function title(c: Chat): string {
	const q = c.messages.find((m) => m.role === 'User')?.text ?? '';
	const line = q.replace(/\s+/g, ' ').trim();
	if (!line) return 'New chat';
	return line.length > 60 ? `${line.slice(0, 59).trimEnd()}…` : line;
}

const DAY = 86_400_000;
const midnight = (t: number) => new Date(t).setHours(0, 0, 0, 0);

/** When a chat was last used: Now, a time today, Yesterday, a weekday, else a date. */
export function when(at: number, now = Date.now()): string {
	if (now - at < 60_000) return 'Now';
	const days = Math.round((midnight(now) - midnight(at)) / DAY);
	const d = new Date(at);
	if (days <= 0) return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
	if (days === 1) return 'Yesterday';
	if (days < 7) return d.toLocaleDateString(undefined, { weekday: 'short' });
	return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

/** "3 messages", "1 message". */
export const count = (n: number) => `${n} message${n === 1 ? '' : 's'}`;
