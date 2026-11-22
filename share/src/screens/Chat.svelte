<script lang="ts">
	import Answer from '../lib/Answer.svelte';
	import type { Status } from '../lib/guest.js';
	import type { Message } from '../lib/store.js';
	import { leftWords } from '../lib/words.js';

	// Board "Share a model", screens 5 and 7: the app's chat without its tabs. The line on top
	// says whose computer it is and what's left today; answers arrive as they are written.
	let {
		status,
		messages,
		writing,
		send
	}: {
		status: Status;
		messages: Message[];
		writing: string | null;
		send: (text: string) => void;
	} = $props();

	let draft = $state('');
	let thread: HTMLElement | undefined = $state();
	const canSend = $derived(draft.trim().length > 0 && writing === null);

	function submit(e: SubmitEvent) {
		e.preventDefault();
		if (!canSend) return;
		send(draft.trim());
		draft = '';
	}

	function keydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
			e.preventDefault();
			(e.currentTarget as HTMLTextAreaElement).form?.requestSubmit();
		}
	}

	$effect(() => {
		void messages.length;
		void writing;
		thread?.scrollTo({ top: thread.scrollHeight });
	});
</script>

<main class="mx-auto flex h-dvh w-full max-w-[760px] flex-col px-4">
	<header class="flex items-end gap-3 pt-5 pb-3">
		<div class="flex min-w-0 flex-1 flex-col gap-0.5">
			<p class="flex items-center gap-2">
				<span class="size-[9px] rounded-full bg-accent-text" aria-hidden="true"></span>
				<span class="truncate text-[20px] font-semibold tracking-[-0.4px]">
					{status.model}
				</span>
			</p>
			<p class="truncate font-mono text-[10px] tracking-[1px] text-text-3 uppercase">
				{status.computer} · private · {leftWords(status)}
			</p>
		</div>
		<a
			class="hidden shrink-0 text-[13px] font-semibold text-text-2 sm:block"
			href="https://pocketgpu.app"
			rel="noopener"
		>
			PocketGPU
		</a>
	</header>

	<section
		bind:this={thread}
		class="flex flex-1 flex-col gap-5 overflow-y-auto py-3"
		aria-live="polite"
	>
		{#if messages.length === 0 && writing === null}
			<p class="m-auto max-w-[320px] text-center text-[15px] text-text-3">
				Ask anything. Your chat stays in this browser.
			</p>
		{/if}
		{#each messages as m, i (i)}
			{#if m.role === 'User'}
				<div class="flex justify-end">
					<p
						class="max-w-[80%] rounded-[18px] rounded-br-[6px] bg-ink px-3.5 py-2.5 text-[15px] leading-[1.4] whitespace-pre-wrap text-on-ink"
					>
						{m.text}
					</p>
				</div>
			{:else}
				<Answer text={m.text} />
			{/if}
		{/each}
		{#if writing !== null}
			{#if writing}<Answer text={writing} />{/if}
			<p class="flex items-center gap-2 text-[13px] text-text-2">
				<span class="size-[7px] animate-pulse rounded-full bg-accent-text"></span> writing…
			</p>
		{/if}
	</section>

	<form class="flex items-end gap-2 pt-2 pb-3" onsubmit={submit}>
		<textarea
			bind:value={draft}
			onkeydown={keydown}
			rows="1"
			maxlength="8000"
			placeholder={`Message ${status.model}`}
			aria-label="Message"
			class="field-sizing-content max-h-40 min-h-[48px] flex-1 resize-none rounded-[16px] border border-border bg-surface px-4 py-3 text-[15px] leading-[1.4] outline-none focus:border-border-strong"
		></textarea>
		<button
			type="submit"
			disabled={!canSend}
			aria-label="Send"
			class="key size-[48px] shrink-0 rounded-full bg-ink text-[20px] text-on-ink disabled:opacity-40"
		>
			↑
		</button>
	</form>
	<p class="pb-4 text-center text-[12px] text-text-3">
		Have a GPU of your own? PocketGPU puts your own AI in your pocket.
		<a class="font-semibold text-text-2" href="https://pocketgpu.app" rel="noopener">
			pocketgpu.app →
		</a>
	</p>
</main>
