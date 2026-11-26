// The page's side of pgpu/model-guest/1 (F48), over the wasm in ../wasm (built by
// wasm/build.sh). Replies arrive as the protocol's own messages turned into plain objects.

import init, { Guest, newSecretKey } from '../wasm/pocketgpu_share_wasm.js';

export type GuestState =
	'Ready' | 'Paused' | 'Ended' | 'ModelUnavailable' | { LimitReached: { resets_ms: number } };

export type Status = {
	computer: string;
	from: string | null;
	model: string;
	ends_ms: number | null;
	limit: number | null;
	used_today: number;
	state: GuestState;
};

export type Refusal =
	| 'NotForYou'
	| 'AlreadyClaimed'
	| 'LinkExpired'
	| 'Ended'
	| 'Paused'
	| 'ModelUnavailable'
	| 'TooLong'
	| 'Busy'
	| 'TooManyTries'
	| { LimitReached: { resets_ms: number } };

/** How fast an answer came (computers from v1.2.42 on). */
export type Speed = { tokens_per_s: number; ms: number };

export type Reply =
	| { Claimed: Status }
	| { Status: Status }
	| { Done: { used_today: number; speed?: Speed } }
	| { Refused: Refusal };

// Loads the wasm once. A load that failed (a dropped connection, say) is forgotten, so the
// next try loads it again instead of failing for good.
let ready: Promise<unknown> | null = null;
const loaded = () =>
	(ready ??= init().catch((e: unknown) => {
		ready = null;
		throw e;
	}));

export async function freshKey(): Promise<number[]> {
	await loaded();
	return Array.from(newSecretKey());
}

export class Connection {
	private constructor(private guest: Guest) {}

	/** Throws with a message starting "unreachable" when no relay could reach the computer. */
	static async open(fragment: string, key: number[]): Promise<Connection> {
		await loaded();
		return new Connection(await Guest.connect(fragment, new Uint8Array(key)));
	}

	peek = async () => (await this.guest.peek()) as Reply;
	claim = async () => (await this.guest.claim()) as Reply;
	status = async () => (await this.guest.status()) as Reply;
	chat = async (messages: { role: string; text: string }[], onText: (t: string) => void) =>
		(await this.guest.chat(messages, onText)) as Reply;
	close = () => this.guest.close();
}
