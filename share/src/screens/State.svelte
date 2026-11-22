<script lang="ts">
	import type { Plain } from '../lib/words.js';

	// Board "Share a model", screen 6: why it can't answer, in words. States that pass by
	// themselves (busy, off, model loading) try again on their own.
	let { model, what, retry }: { model: string | null; what: Plain; retry: (() => void) | null } =
		$props();
</script>

<main class="mx-auto flex min-h-dvh w-full max-w-[560px] flex-col gap-5 px-4 pt-10 pb-6">
	<p class="text-right font-mono text-[10px] tracking-[1px] text-text-3 uppercase">
		Shared with you
	</p>
	{#if model}
		<h1 class="text-[28px] leading-tight font-semibold tracking-[-0.6px]">{model}</h1>
	{/if}
	<section class="flex flex-col gap-2 rounded-[22px] bg-display p-4" aria-live="polite">
		<p class="font-mono text-[10px] tracking-[1px] text-display-dim uppercase">
			{what.retry ? 'Trying again by itself' : 'Can’t answer'}
		</p>
		<p class="text-[20px] font-semibold text-display-text">{what.title}</p>
	</section>
	<p class="text-[15px] leading-[1.4] text-text-2">{what.body}</p>
	<p class="text-[13px] leading-[1.4] text-text-3">Your chat is kept in this browser.</p>
	{#if retry}
		<button
			type="button"
			class="key mt-auto h-[52px] w-full rounded-[14px] bg-ink text-[15px] font-semibold text-on-ink"
			onclick={retry}
		>
			Try again now
		</button>
	{/if}
</main>
