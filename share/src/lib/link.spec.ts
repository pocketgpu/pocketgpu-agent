import { describe, expect, it } from 'vitest';
import { readFragment, storageId } from './link.js';

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
