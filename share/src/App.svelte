<script lang="ts">
	import { Connection, freshKey, type Refusal, type Reply, type Status } from './lib/guest.js';
	import { readFragment } from './lib/link.js';
	import {
		getChat,
		getLink,
		lastLink,
		putChat,
		putLink,
		context,
		type Message,
		type Saved
	} from './lib/store.js';
	import { plain, unreachable, type Plain } from './lib/words.js';
	import Chat from './screens/Chat.svelte';
	import State from './screens/State.svelte';
	import Welcome from './screens/Welcome.svelte';

	// share.pocketgpu.app (F48): the link's fragment, this browser's key and its chat are kept
	// here only. Opening shows what was shared (Peek, nothing claimed, so a link preview can't
	// use it up); Start chatting claims it; later visits go straight to the chat.
	type View =
		| { v: 'loading' }
		| { v: 'nolink' }
		| { v: 'welcome' }
		| { v: 'chat' }
		| { v: 'state'; what: Plain };

	let view: View = $state({ v: 'loading' });
	let status: Status | null = $state(null);
	let messages: Message[] = $state([]);
	let writing: string | null = $state(null);
	let busy = $state(false);
	let saved: Saved | null = null;
	let conn: Connection | null = null;
	let timer: ReturnType<typeof setTimeout> | undefined;
	/** A message the computer couldn't take (paused, offline): sent again once it can. */
	let pending: string | null = null;

	function show(r: Refusal) {
		const what = plain(r, status);
		view = { v: 'state', what };
		if (what.retry) later();
	}

	function later() {
		clearTimeout(timer);
		timer = setTimeout(() => void resume(), 20_000);
	}

	async function connect(): Promise<Connection | null> {
		if (conn) return conn;
		try {
			conn = await Connection.open(saved!.fragment, saved!.key);
			return conn;
		} catch {
			view = { v: 'state', what: unreachable(status) };
			later();
			return null;
		}
	}

	function take(reply: Reply): boolean {
		if ('Claimed' in reply) status = reply.Claimed;
		else if ('Status' in reply) status = reply.Status;
		else if ('Refused' in reply) {
			show(reply.Refused);
			return false;
		}
		return true;
	}

	/** Opens the saved link: the chat when this browser claimed it, else the welcome page. */
	async function resume() {
		const c = await connect();
		if (!c) return;
		try {
			const reply = saved!.claimed ? await c.status() : await c.peek();
			if (!take(reply)) return;
			const state = status!.state;
			if (saved!.claimed && state !== 'Ready') return show(state as Refusal);
			view = saved!.claimed ? { v: 'chat' } : { v: 'welcome' };
			if (saved!.claimed && pending) {
				const text = pending;
				pending = null;
				void send(text);
			}
		} catch {
			conn = null;
			view = { v: 'state', what: unreachable(status) };
			later();
		}
	}

	async function start() {
		busy = true;
		const c = await connect();
		if (c) {
			try {
				if (take(await c.claim())) {
					saved = { ...saved!, claimed: true };
					await putLink($state.snapshot(saved));
					view = { v: 'chat' };
				}
			} catch {
				conn = null;
				view = { v: 'state', what: unreachable(status) };
			}
		}
		busy = false;
	}

	/** The message goes back off the chat, to be sent again when the computer can take it. */
	async function holdBack(text: string) {
		messages = messages.slice(0, -1);
		pending = text;
		await putChat(saved!.id, $state.snapshot(messages));
	}

	async function send(text: string) {
		const c = await connect();
		if (!c) return void (pending = text);
		messages = [...messages, { role: 'User', text, at: Date.now() }];
		await putChat(saved!.id, $state.snapshot(messages));
		writing = '';
		try {
			const reply = await c.chat(context(messages), (t) => (writing = (writing ?? '') + t));
			if ('Done' in reply) {
				messages = [
					...messages,
					{
						role: 'Assistant',
						text: writing ?? '',
						at: Date.now(),
						...(reply.Done.speed && { speed: reply.Done.speed })
					}
				];
				status = { ...status!, used_today: reply.Done.used_today };
				await putChat(saved!.id, $state.snapshot(messages));
			} else if ('Refused' in reply) {
				const r = reply.Refused;
				// Waiting states keep the question for later; TooLong and the like drop it.
				if (r === 'Paused' || r === 'Busy' || r === 'ModelUnavailable')
					await holdBack(text);
				show(r);
			}
		} catch {
			// The connection went (stopped, computer off): ask again on a new one, which says why.
			writing = null;
			await holdBack(text);
			conn = null;
			await resume();
		}
		writing = null;
	}

	/** While the chat is open, ask now and then, so a stop or a pause shows within 10 s. */
	async function check() {
		if (!conn || writing !== null || view.v !== 'chat') return;
		try {
			const reply = await conn.status();
			if (take(reply) && status!.state !== 'Ready') show(status!.state as Refusal);
		} catch {
			conn = null;
			await resume();
		}
	}

	$effect(() => {
		if (view.v !== 'chat') return;
		const t = setInterval(() => void check(), 8_000);
		return () => clearInterval(t);
	});

	async function boot() {
		const frag = readFragment(location.hash);
		// The secret stays out of the address bar, history and screenshots from here on.
		if (location.hash) history.replaceState(null, '', location.pathname);
		const found = frag ? await getLink(frag.id) : await lastLink();
		if (found) saved = found;
		else if (frag) {
			saved = { id: frag.id, fragment: frag.raw, key: await freshKey(), claimed: false };
			await putLink(saved);
		} else {
			view = { v: 'nolink' };
			return;
		}
		messages = await getChat(saved.id);
		await resume();
	}

	void boot();
</script>

{#if view.v === 'loading'}
	<main class="flex min-h-dvh items-center justify-center text-[15px] text-text-3">
		Connecting…
	</main>
{:else if view.v === 'nolink'}
	<main class="mx-auto flex min-h-dvh max-w-[560px] flex-col justify-center gap-3 px-4">
		<h1 class="text-[28px] font-semibold tracking-[-0.6px]">A shared AI model</h1>
		<p class="text-[15px] leading-[1.4] text-text-2">
			This page opens from a link someone sends you. Open the link they sent, on this device.
		</p>
		<a class="text-[15px] font-semibold" href="https://pocketgpu.app" rel="noopener">
			What is PocketGPU? →
		</a>
	</main>
{:else if view.v === 'welcome'}
	<Welcome {status} {busy} {start} />
{:else if view.v === 'chat' && status}
	<Chat {status} {messages} {writing} {send} />
{:else if view.v === 'state'}
	<State
		model={status?.model ?? null}
		what={view.what}
		retry={view.what.retry ? () => void resume() : null}
	/>
{/if}
