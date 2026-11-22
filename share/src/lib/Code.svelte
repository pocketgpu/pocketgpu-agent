<script lang="ts">
	import { highlight, type Tone } from './highlight.js';

	// Copied from app/src/lib/chat/Code.svelte (F48): a code block on the black display, its
	// language and a copy key, coloured for the languages highlight.ts knows.
	let { lang, code }: { lang: string; code: string } = $props();
	const pieces = $derived(highlight(code, lang));
	const TONE: Record<Tone, string> = {
		kw: 'text-code-kw',
		str: 'text-code-str',
		num: 'text-code-num',
		fn: 'text-code-fn',
		com: 'text-code-com'
	};

	let copied = $state(false);
	async function copy() {
		try {
			await navigator.clipboard.writeText(code);
		} catch {
			return;
		}
		copied = true;
		setTimeout(() => (copied = false), 1500);
	}
</script>

<section
	class="overflow-hidden rounded-[16px] border border-display-edge bg-display"
	aria-label={lang || 'Code'}
>
	<header class="flex items-center gap-2 border-b border-display-grid py-1 pr-1.5 pl-3.5">
		<span
			class="min-w-0 flex-1 truncate font-mono text-[10px] tracking-[1px] text-display-dim uppercase"
		>
			{lang || 'code'}
		</span>
		<button
			type="button"
			class="rounded-[10px] px-2 py-1.5 font-mono text-[10px] tracking-[1px] text-display-dim uppercase active:opacity-60"
			onclick={copy}
		>
			{copied ? 'Copied' : 'Copy'}
		</button>
	</header>
	<pre
		class="overflow-x-auto overscroll-x-contain px-3.5 pt-3 pb-3.5 font-mono text-[12.5px] leading-[1.7] text-code-text select-text"><code
			>{#each pieces as p, i (i)}<span class={p.tone && TONE[p.tone]}>{p.v}</span
				>{/each}</code
		></pre>
</section>
