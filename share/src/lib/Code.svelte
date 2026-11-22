<script lang="ts">
	import Icon from './Icon.svelte';
	import { guess, highlight, type Tone } from './highlight.js';

	// Copied from app/src/lib/chat/Code.svelte (F48): a code block on the black display, its
	// language and a copy key, coloured for the languages highlight.ts knows.
	let { lang, code }: { lang: string; code: string } = $props();
	// A block without a language gets the one its code looks like.
	const shown = $derived(lang || guess(code));
	const pieces = $derived(highlight(code, shown));
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
	aria-label={shown || 'Code'}
>
	<header class="flex items-center gap-2 border-b border-display-grid py-1 pr-1.5 pl-3.5">
		<span
			class="min-w-0 flex-1 truncate font-mono text-[10px] tracking-[1px] text-display-dim uppercase"
		>
			{shown || 'code'}
		</span>
		<button
			type="button"
			aria-label={copied ? 'Copied' : 'Copy code'}
			class="relative flex size-8 shrink-0 items-center justify-center rounded-[10px] text-display-dim after:absolute after:-inset-1.5 active:opacity-60"
			onclick={copy}
		>
			<Icon name={copied ? 'check' : 'copy'} class={copied ? 'text-display-accent' : ''} />
		</button>
	</header>
	<pre
		class="overflow-x-auto overscroll-x-contain px-3.5 pt-3 pb-3.5 font-mono text-[12.5px] leading-[1.7] text-code-text select-text"><code
			>{#each pieces as p, i (i)}<span class={p.tone && TONE[p.tone]}>{p.v}</span
				>{/each}</code
		></pre>
</section>
