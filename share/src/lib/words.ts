// Plain words for what the computer says (F48, board "Share a model", screens 4–6).
import type { GuestState, Refusal, Status } from './guest.js';

export type Plain = { title: string; body: string; retry: boolean };

const time = (ms: number) =>
	new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
const day = (ms: number) =>
	new Date(ms).toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'short' });

/** Who shared it, for the words: their name, else the computer's. */
export const who = (s: Status) => s.from ?? s.computer;

export function untilWords(s: Status): string {
	return s.ends_ms
		? `Until ${day(s.ends_ms)}. Then it ends by itself`
		: 'Until they stop sharing it';
}

export function leftWords(s: Status, used = s.used_today): string {
	return s.limit === null ? 'no daily limit' : `${Math.max(0, s.limit - used)} left today`;
}

export function plain(r: Refusal | GuestState, s?: Status | null): Plain {
	const name = s ? who(s) : 'Whoever shared it';
	const computer = s?.computer ?? 'The computer';
	if (typeof r === 'object' && 'LimitReached' in r) {
		return {
			title: "Today's messages are used",
			body: `They come back at ${time(r.LimitReached.resets_ms)}.`,
			retry: false
		};
	}
	switch (r) {
		case 'Paused':
			return {
				title: `${computer} is busy right now`,
				body: `${name}'s computer is busy with its own work, so the AI is resting for now. This page tries again by itself; keep it open.`,
				retry: true
			};
		case 'ModelUnavailable':
			return {
				title: "This model isn't available right now",
				body: `It may be turned off or loading on ${computer}. This page tries again by itself.`,
				retry: true
			};
		case 'Ended':
		case 'NotForYou':
			return {
				title: 'This link has ended',
				body: `Ask ${name} for a new one if you'd like to keep chatting.`,
				retry: false
			};
		case 'AlreadyClaimed':
			return {
				title: 'This link was already used',
				body: 'A link works for one person, in the browser that opened it first. Ask for a new one.',
				retry: false
			};
		case 'LinkExpired':
			return {
				title: 'This link has run out',
				body: 'A link has to be opened within a week. Ask for a new one.',
				retry: false
			};
		case 'TooManyTries':
			return {
				title: 'Too many tries',
				body: 'Wait an hour, then open the link again.',
				retry: false
			};
		default:
			return { title: 'Something went wrong', body: 'Try again in a moment.', retry: true };
	}
}

export const unreachable = (s?: Status | null): Plain => ({
	title: `${s?.computer ?? 'The computer'} can't be reached right now`,
	body: 'It may be off or offline. This page tries again by itself; your chat is kept in this browser.',
	retry: true
});

/** A link the page can't read: part of it is missing or changed. Trying again won't help. */
export const invalidLink: Plain = {
	title: "This link isn't valid",
	body: 'Part of it may be missing. Open the whole link again, or ask for a new one.',
	retry: false
};
