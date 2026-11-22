import { describe, expect, it } from 'vitest';
import {
	count,
	fresh,
	fromSaved,
	MAX_CHATS,
	newest,
	openMessages,
	title,
	when,
	withMessages,
	type Chats
} from './chats.js';
import type { Message } from './store.js';

const q = (text: string, at = 0): Message => ({ role: 'User', text, at });

describe('chats', () => {
	it('reads the one chat kept before there were several', () => {
		const old = fromSaved([q('hi', 5), { role: 'Assistant', text: 'hello', at: 6 }]);
		expect(old.chats).toHaveLength(1);
		expect(old.chats[0].at).toBe(6);
		expect(openMessages(old)).toHaveLength(2);
		expect(fromSaved(undefined).chats).toEqual([]);
		expect(fromSaved([]).chats).toEqual([]);
	});

	it('lists a chat once it has a message, and keeps the old one on New chat', () => {
		let all: Chats = { open: 'a', chats: [] };
		all = withMessages(all, [], 1);
		expect(all.chats).toEqual([]);
		all = withMessages(all, [q('first')], 2);
		all = fresh(all);
		expect(openMessages(all)).toEqual([]);
		all = withMessages(all, [q('second')], 3);
		expect(newest(all.chats).map(title)).toEqual(['second', 'first']);
		all = { ...all, open: 'a' };
		expect(openMessages(all)[0].text).toBe('first');
	});

	it('lets the oldest go past the limit, never the open one', () => {
		const chats = Array.from({ length: MAX_CHATS + 5 }, (_, i) => ({
			id: `c${i}`,
			messages: [q(`q${i}`)],
			at: i
		}));
		const all = withMessages({ open: 'c0', chats }, [q('again')], 1000);
		expect(all.chats).toHaveLength(MAX_CHATS);
		expect(all.chats.some((c) => c.id === 'c0')).toBe(true);
		expect(all.chats.some((c) => c.id === 'c1')).toBe(false);
	});

	it('names a chat by its first question, on one line', () => {
		expect(title({ id: 'x', at: 0, messages: [q('  How do\nI   bake? ')] })).toBe(
			'How do I bake?'
		);
		expect(title({ id: 'x', at: 0, messages: [] })).toBe('New chat');
		expect(title({ id: 'x', at: 0, messages: [q('a'.repeat(100))] })).toHaveLength(60);
	});

	it('says when in words', () => {
		const now = new Date(2026, 10, 22, 15, 0).getTime();
		expect(when(now - 10_000, now)).toBe('Now');
		expect(when(new Date(2026, 10, 21, 9, 0).getTime(), now)).toBe('Yesterday');
		expect(when(new Date(2026, 10, 2).getTime(), now)).toMatch(/2/);
		expect(count(1)).toBe('1 message');
		expect(count(3)).toBe('3 messages');
	});
});
