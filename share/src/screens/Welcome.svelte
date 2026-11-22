<script lang="ts">
	import type { Status } from '../lib/guest.js';
	import { leftWords, untilWords, who } from '../lib/words.js';

	// Board "Share a model", screen 4: what was shared, by whom, what stays private, when it
	// ends; Start chatting claims the link for this browser.
	let { status, busy, start }: { status: Status | null; busy: boolean; start: () => void } =
		$props();
	const name = $derived(status ? who(status) : null);
</script>

<main class="mx-auto flex min-h-dvh w-full max-w-[560px] flex-col gap-5 px-4 pt-10 pb-6">
	<p class="text-right font-mono text-[10px] tracking-[1px] text-text-3 uppercase">
		Shared with you
	</p>
	<header class="flex flex-col gap-1.5">
		<h1 class="text-[28px] leading-tight font-semibold tracking-[-0.6px]">
			{status?.model ?? 'A shared AI model'}
		</h1>
		<p class="text-[15px] leading-[1.4] text-text-2">
			{#if status?.from}
				{status.from} shared an AI model with you. Chat with it right here.
			{:else if status}
				{status.computer} is shared with you. Chat with its AI model right here.
			{:else}
				Someone shared an AI model with you. Chat with it right here.
			{/if}
		</p>
	</header>

	{#if status}
		<section class="flex flex-col gap-3 rounded-[22px] bg-display p-4" aria-label="The model">
			<p class="font-mono text-[10px] tracking-[1px] text-display-dim uppercase">
				On {status.computer} · ready
			</p>
			<div class="flex gap-8">
				<div class="flex flex-col">
					<span class="text-[22px] font-semibold text-display-text">{status.model}</span>
					<span class="font-mono text-[10px] tracking-[1px] text-display-dim uppercase">
						Model
					</span>
				</div>
				<div class="flex flex-col">
					<span class="text-[22px] font-semibold text-display-text tabular-nums">
						{status.limit ?? '∞'}
					</span>
					<span class="font-mono text-[10px] tracking-[1px] text-display-dim uppercase">
						Messages a day
					</span>
				</div>
			</div>
		</section>
	{/if}

	<ul class="flex flex-col rounded-[20px] border border-border bg-surface px-4">
		<li class="py-3.5 text-[15px] leading-[1.4]">
			Nothing to install. It works in this browser
		</li>
		<li class="border-t border-border py-3.5 text-[15px] leading-[1.4]">
			Private: your messages go only to {status?.computer ?? 'their computer'}, encrypted the
			whole way
		</li>
		{#if status}
			<li class="border-t border-border py-3.5 text-[15px] leading-[1.4]">
				{untilWords(status)} · {leftWords(status)}
			</li>
		{/if}
		<li class="border-t border-border py-3.5 text-[15px] leading-[1.4]">
			PocketGPU never shows {name ?? 'them'} what you ask. {name ?? 'They'} see{name
				? 's'
				: ''} only how many messages you send
		</li>
	</ul>

	<div class="mt-auto flex flex-col gap-3">
		<button
			type="button"
			class="key h-[52px] w-full rounded-[14px] bg-ink text-[15px] font-semibold text-on-ink disabled:opacity-50"
			disabled={busy}
			onclick={start}
		>
			{busy ? 'Connecting…' : 'Start chatting'}
		</button>
		<p class="text-center text-[13px] text-text-3">
			Have a GPU of your own? PocketGPU puts your own AI in your pocket.
			<a class="font-semibold text-text-2" href="https://pocketgpu.app" rel="noopener">
				pocketgpu.app →
			</a>
		</p>
	</div>
</main>
