<script lang="ts">
	// The few Lucide icons the page uses, drawn inline (no icon package, nothing fetched).
	let {
		name,
		size = 15,
		class: cls = ''
	}: {
		name:
			| 'copy'
			| 'check'
			| 'arrow-down'
			| 'history'
			| 'plus'
			| 'message-circle'
			| 'chevron-right';
		size?: number;
		class?: string;
	} = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width="2"
	stroke-linecap="round"
	stroke-linejoin="round"
	class={cls}
	aria-hidden="true"
>
	{#if name === 'copy'}
		<rect x="8" y="8" width="14" height="14" rx="2" ry="2" />
		<path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
	{:else if name === 'check'}
		<path d="M20 6 9 17l-5-5" />
	{:else if name === 'history'}
		<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
		<path d="M3 3v5h5" />
		<path d="M12 7v5l4 2" />
	{:else if name === 'plus'}
		<path d="M5 12h14" />
		<path d="M12 5v14" />
	{:else if name === 'message-circle'}
		<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z" />
	{:else if name === 'chevron-right'}
		<path d="m9 18 6-6-6-6" />
	{:else}
		<path d="M12 5v14" />
		<path d="m19 12-7 7-7-7" />
	{/if}
</svg>
