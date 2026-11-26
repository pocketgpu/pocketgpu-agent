import { describe, expect, it } from 'vitest';
import { badLink, readFragment, storageId } from './link.js';

describe('readFragment', () => {
	it('reads a link fragment and names it without the secret', () => {
		const f = readFragment('#AQcHBwcH_-abc');
		expect(f?.raw).toBe('AQcHBwcH_-abc');
		expect(f?.id).toBe(storageId('AQcHBwcH_-abc'));
		expect(f?.id).not.toContain('AQcH');
	});
	it('refuses what cannot be a link', () => {
		expect(readFragment('')).toBeNull();
		expect(readFragment('#')).toBeNull();
		expect(readFragment('#has space')).toBeNull();
		expect(readFragment('#' + 'A'.repeat(601))).toBeNull();
	});
});

describe('badLink', () => {
	it("knows a link that can't be read, which trying again won't fix", () => {
		expect(badLink(new Error('this link is damaged'))).toBe(true);
		expect(badLink(new Error('this link is for a newer PocketGPU'))).toBe(true);
		expect(badLink(new Error('this link has no way to reach the computer'))).toBe(true);
		expect(badLink(new Error('data is not a valid public key'))).toBe(true);
		expect(badLink(new Error("this browser's key is damaged"))).toBe(true);
	});
	it('leaves a computer that is off, or a page that did not load, to try again', () => {
		expect(badLink(new Error('unreachable: no relay'))).toBe(false);
		expect(badLink(new TypeError('Failed to fetch'))).toBe(false);
		expect(badLink('unreachable: timed out')).toBe(false);
	});
});
