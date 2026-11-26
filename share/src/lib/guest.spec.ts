import { describe, expect, it, vi } from 'vitest';

// The wasm stands in: its first load fails, the next one works.
const wasm = vi.hoisted(() => ({
	init: vi.fn(),
	newSecretKey: vi.fn(() => new Uint8Array([7, 8, 9]))
}));
vi.mock('../wasm/pocketgpu_share_wasm.js', () => ({
	default: wasm.init,
	newSecretKey: wasm.newSecretKey,
	Guest: class {}
}));

import { freshKey } from './guest.js';

describe('guest', () => {
	it('loads the wasm again after a failed load', async () => {
		wasm.init.mockRejectedValueOnce(new Error('offline')).mockResolvedValue(undefined);
		await expect(freshKey()).rejects.toThrow('offline');
		expect(await freshKey()).toEqual([7, 8, 9]);
		expect(await freshKey()).toEqual([7, 8, 9]);
		// Once loaded, it isn't loaded again.
		expect(wasm.init).toHaveBeenCalledTimes(2);
	});
});
