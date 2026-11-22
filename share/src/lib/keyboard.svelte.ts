// Copied from app/src/lib/chat/keyboard.svelte.ts (F48), so the guest page behaves like the app.
// On a phone the keyboard covers the bottom of the page and iOS keeps the page's height, so a
// screen with a text box at the bottom shrinks to what is still visible (as Sheet.svelte does
// for sheets). Computers' windows have no such keyboard.

export class Keyboard {
	/** How many pixels at the bottom of the page the keyboard covers now. */
	covered = $state(0);

	/** Follows the keyboard until the returned function is called; for an `$effect`. */
	watch(onchange?: () => void): (() => void) | undefined {
		const vv = window.visualViewport;
		if (!vv || matchMedia('(min-width: 64rem)').matches) return;
		const fit = () => {
			const covered = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
			if (covered === this.covered) return;
			this.covered = covered;
			onchange?.();
		};
		fit();
		vv.addEventListener('resize', fit);
		vv.addEventListener('scroll', fit);
		return () => {
			vv.removeEventListener('resize', fit);
			vv.removeEventListener('scroll', fit);
			this.covered = 0;
		};
	}
}
