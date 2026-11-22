import 'fake-indexeddb/auto';
import { describe, expect, it } from 'vitest';
import {
	context,
	getChat,
	getLink,
	lastLink,
	MAX_MESSAGES,
	putChat,
	putLink,
	type Message
} from './store.js';

const msg = (role: Message['role'], text: string): Message => ({ role, text, at: 0 });

describe('store', () => {
	it('keeps the link, its key and the chat across visits', async () => {
		await putLink({ id: 'l1', fragment: 'AAA', key: [1, 2, 3], claimed: true });
		await putChat('l1', [msg('User', 'hi'), msg('Assistant', 'hello')]);
		expect((await getLink('l1'))?.key).toEqual([1, 2, 3]);
		expect((await lastLink())?.id).toBe('l1');
		expect(await getChat('l1')).toHaveLength(2);
		expect(await getChat('nope')).toEqual([]);
	});
});

describe('context', () => {
	it('sends the newest messages that fit, starting with a question', () => {
		const many = Array.from({ length: 60 }, (_, i) =>
			msg(i % 2 ? 'Assistant' : 'User', `m${i}`)
		);
		many.push(msg('User', 'last'));
		const out = context(many);
		expect(out.length).toBeLessThanOrEqual(MAX_MESSAGES);
		expect(out.at(-1)?.text).toBe('last');
		expect(out[0].role).toBe('User');
	});
	it('stays under 64 KB and cuts a message too long on its own', () => {
		const big = Array.from({ length: 12 }, () => msg('User', 'x'.repeat(9000)));
		const out = context(big);
		const bytes = out.reduce((n, m) => n + m.text.length, 0);
		expect(bytes).toBeLessThanOrEqual(64 * 1024);
		expect(out.every((m) => m.text.length <= 8 * 1024)).toBe(true);
	});
});
