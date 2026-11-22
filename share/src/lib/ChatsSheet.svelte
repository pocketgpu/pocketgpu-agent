<script lang="ts">
	import { count, newest, title, when, type Chats } from './chats.js';
	import Icon from './Icon.svelte';

	// Board "Share a model", slot "Your chats": the earlier chats, newest first, and New chat.
	// A native dialog, so Escape, focus and the page behind are the browser's own.
	let {
		open = $bindable(false),
		chats,
		owner,
		newChat,
		openChat
	}: {
		open: boolean;
		chats: Chats;
		/** Whose computer it is, for the line at the bottom. */
		owner: string;
		newChat: () => void;
		openChat: (id: string) => void;
	} = $props();

	let dialog: HTMLDialogElement | undefined = $state();
	const list = $derived(newest(chats.chats));
	let now = $state(Date.now());

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			now = Date.now();
			dialog.showModal();
		} else if (!open && dialog.open) dialog.close();
	});

	function pick(f: () => void) {
		open = false;
		f();
	}
</script>

<dialog
	bind:this={dialog}
	onclose={() => (open = false)}
	onclick={(e) => e.target === dialog && (open = false)}
	aria-label="Your chats"
	class="mx-auto mt-auto mb-0 max-h-[85dvh] w-full max-w-[560px] rounded-t-[28px] border-t border-border bg-bg p-0 text-text backdrop:bg-[#15151366]"
>
	<div class="flex flex-col gap-4 px-4 pt-2.5 pb-[max(24px,env(safe-area-inset-bottom))]">
		<span class="mx-auto h-[5px] w-10 rounded-full bg-border-strong" aria-hidden="true"></span>
		<div class="flex items-center gap-3">
			<h2 class="flex-1 text-[20px] font-semibold tracking-[-0.4px]">Your chats</h2>
			<button
				type="button"
				class="key flex h-10 items-center gap-1.5 rounded-[12px] bg-ink px-3.5 text-[14px] font-semibold text-on-ink"
				onclick={() => pick(newChat)}
			>
				<Icon name="plus" size={16} /> New chat
			</button>
		</div>
		{#if list.length}
			<ul class="flex flex-col rounded-[20px] border border-border bg-surface px-4">
				{#each list as c (c.id)}
					{@const here = c.id === chats.open}
					<li class="border-b border-border last:border-b-0">
						<button
							type="button"
							aria-current={here ? 'true' : undefined}
							class="flex min-h-[62px] w-full items-center gap-3 py-2.5 text-left active:opacity-70"
							onclick={() => pick(() => openChat(c.id))}
						>
							<Icon
								name="message-circle"
								size={16}
								class={here ? 'shrink-0 text-text' : 'shrink-0 text-text-2'}
							/>
							<span class="flex min-w-0 flex-1 flex-col gap-1">
								<span class="truncate text-[15px] font-semibold">{title(c)}</span>
								<span class="font-mono text-[11.5px] text-text-2">
									{when(c.at, now)} · {count(c.messages.length)}
								</span>
							</span>
							{#if here}
								<span class="font-mono text-[10px] tracking-[1px] text-text-2">
									OPEN
								</span>
							{:else}
								<Icon name="chevron-right" size={16} class="shrink-0 text-text-3" />
							{/if}
						</button>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-[15px] text-text-2">No chats yet. Ask something to start one.</p>
		{/if}
		<p class="text-[13px] leading-[1.35] text-text-2">
			Kept only in this browser. PocketGPU never shows {owner} what you ask.
		</p>
	</div>
</dialog>
