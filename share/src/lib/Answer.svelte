<script lang="ts">
	import Code from './Code.svelte';
	import { parse, type Block, type Inline } from './markdown.js';

	// Copied from app/src/lib/chat/Answer.svelte (F48): a model's answer drawn from its Markdown (board 80): headings, paragraphs, lists, code,
	// tables and quotes made of the app's own elements, never of HTML from the answer. Read again
	// as each piece arrives.
	let { text }: { text: string } = $props();
	const parsed = $derived(parse(text));

	// # is the largest heading, ## the size of board 80's titles, the rest a bold line.
	const SIZE = ['text-[20px]', 'text-[18px]'];
</script>

{#snippet inlines(nodes: Inline[])}
	{#each nodes as n, k (k)}
		{#if n.t === 'text'}{n.v}{:else if n.t === 'br'}<br />{:else if n.t === 'b'}<strong
				class="font-semibold"
			>
				{@render inlines(n.c)}
			</strong>{:else if n.t === 'i'}<em>
				{@render inlines(n.c)}
			</em>{:else if n.t === 'code'}<code
				class="rounded-[5px] bg-surface-2 px-[5px] py-px font-mono text-[13.5px]"
			>
				{n.v}
			</code>{:else}<a
				href={n.href}
				class="font-semibold text-text underline underline-offset-2"
				target="_blank"
				rel="noopener noreferrer nofollow"
			>
				{@render inlines(n.c)}
			</a>{/if}
	{/each}
{/snippet}

{#snippet blocks(list: Block[])}
	{#each list as b, k (k)}
		{#if b.t === 'p'}
			<p>{@render inlines(b.c)}</p>
		{:else if b.t === 'h'}
			<svelte:element
				this={`h${Math.min(b.level + 1, 6)}`}
				class={['leading-[1.3] font-semibold', SIZE[b.level - 1] ?? 'text-base']}
			>
				{@render inlines(b.c)}
			</svelte:element>
		{:else if b.t === 'code'}
			<Code lang={b.lang} code={b.v} />
		{:else if b.t === 'list'}
			<svelte:element
				this={b.ordered ? 'ol' : 'ul'}
				start={b.ordered ? b.start : undefined}
				class={['flex flex-col gap-2', b.ordered ? 'pl-0.5' : 'pl-1']}
			>
				{#each b.items as item, j (j)}
					<li class={['flex', b.ordered ? 'gap-2' : 'gap-2.5']}>
						{#if b.ordered}
							<span class="shrink-0 font-semibold text-text-2 tabular-nums">
								{b.start + j}.
							</span>
						{:else}
							<span
								class="flex h-[1.5em] w-1.5 shrink-0 items-center justify-center"
								aria-hidden="true"
							>
								<span class="size-[5px] rounded-full bg-text-2"></span>
							</span>
						{/if}
						<div class="flex min-w-0 flex-1 flex-col gap-1.5">
							{@render blocks(item)}
						</div>
					</li>
				{/each}
			</svelte:element>
		{:else if b.t === 'quote'}
			<blockquote
				class="flex flex-col gap-1.5 border-l-[3px] border-border-strong py-0.5 pl-3 text-text-2"
			>
				{@render blocks(b.c)}
			</blockquote>
		{:else if b.t === 'table'}
			<div class="overflow-x-auto overscroll-x-contain rounded-[12px] border border-border">
				<table class="min-w-full border-collapse text-left text-[13.5px] leading-snug">
					<thead class="bg-surface-2">
						<tr class="border-b border-border">
							{#each b.head as cell, j (j)}
								<th
									class="px-1 py-[9px] align-top font-semibold text-text-2 first:pl-3 last:pr-3"
								>
									{@render inlines(cell)}
								</th>
							{/each}
						</tr>
					</thead>
					<tbody>
						{#each b.rows as row, r (r)}
							<tr
								class={[
									'bg-surface',
									r < b.rows.length - 1 && 'border-b border-border'
								]}
							>
								{#each row as cell, j (j)}
									<td class="px-1 py-[9px] align-top first:pl-3 last:pr-3">
										{@render inlines(cell)}
									</td>
								{/each}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{:else}
			<hr class="border-border" />
		{/if}
	{/each}
{/snippet}

<div
	class="flex min-w-0 flex-col gap-3 text-[15px] leading-[1.45] break-words text-text select-text"
>
	{@render blocks(parsed)}
</div>
