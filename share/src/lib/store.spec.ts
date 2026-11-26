import 'fake-indexeddb/auto';
import { describe, expect, it } from 'vitest';
import {
	context,
	forgetLink,
	getChats,
	getLink,
	lastLink,
	MAX_MESSAGES,
	putChats,
	putLink,
	type Message
} from './store.js';

const msg = (role: Message['role'], text: string): Message => ({ role, text, at: 0 });

describe('store', () => {
	it('keeps the link, its key and the chat across visits', async () => {
		await putLink({ id: 'l1', fragment: 'AAA', key: [1, 2, 3], claimed: true });
		await putChats('l1', {
			open: 'c1',
			chats: [{ id: 'c1', messages: [msg('User', 'hi')], at: 1 }]
		});
		expect((await getLink('l1'))?.key).toEqual([1, 2, 3]);
		expect((await lastLink())?.id).toBe('l1');
		expect((await getChats('l1')).chats[0].messages).toHaveLength(1);
		expect((await getChats('nope')).chats).toEqual([]);
	});
	it('reopens the link opened last, not the one whose id sorts last', async () => {
		// Ended link first, the new one after it; their ids sort the other way round.
		await putLink({ id: 'lzz', fragment: 'OLD', key: [1], claimed: true, opened: 1_000 });
		await putLink({ id: 'laa', fragment: 'NEW', key: [2], claimed: true, opened: 2_000 });
		expect((await lastLink())?.id).toBe('laa');
		// Opening the old one again makes it the last one.
		await putLink({ id: 'lzz', fragment: 'OLD', key: [1], claimed: true, opened: 3_000 });
		expect((await lastLink())?.id).toBe('lzz');
	});
	it('forgets a link that cannot be read', async () => {
		await putLink({ id: 'lbad', fragment: 'BAD', key: [3], claimed: false, opened: 9_000 });
		expect((await lastLink())?.id).toBe('lbad');
		await forgetLink('lbad');
		expect(await getLink('lbad')).toBeUndefined();
		expect((await lastLink())?.id).toBe('lzz');
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
