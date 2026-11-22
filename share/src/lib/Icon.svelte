<script lang="ts">
	// The few Lucide icons the page uses, drawn inline (no icon package, nothing fetched).
	let {
		name,
		size = 15,
		class: cls = ''
	}: { name: 'copy' | 'check' | 'arrow-down'; size?: number; class?: string } = $props();
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
	{:else}
		<path d="M12 5v14" />
		<path d="m19 12-7 7-7-7" />
	{/if}
</svg>
