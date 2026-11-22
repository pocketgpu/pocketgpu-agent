// Copied from app/src/lib/chat/follow.svelte.ts (F48), so the guest page behaves like the app.
// A chat's messages follow new text only while the person is at the bottom. Any move up stops
// it at once (an answer comes faster than a finger gets far), nothing moves under a finger, and
// the view stays put with a way back (Latest) until they're at the bottom again.

const NEAR_PX = 48;

export class Follow {
	/** The scrolling list, bound with `bind:this`. */
	list: HTMLElement | undefined = undefined;
	following = $state(true);
	touching = false;
	private lastTop = 0;

	onscroll = () => {
		const list = this.list;
		if (!list) return;
		const top = list.scrollTop;
		const end = list.scrollHeight - list.clientHeight;
		// Up, and not iOS bouncing back from past the end.
		if (top < this.lastTop - 1 && top < end - 1) this.following = false;
		else if (end - top < NEAR_PX && !this.touching) this.following = true;
		this.lastTop = top;
	};

	// A wheel or trackpad turned up stops following before its scroll lands, which a new
	// piece of the answer would otherwise undo.
	onwheel = (e: WheelEvent) => {
		if (e.deltaY < 0) this.following = false;
	};

	ontouchstart = () => (this.touching = true);
	ontouchend = () => ((this.touching = false), this.onscroll());
	ontouchcancel = () => (this.touching = false);

	/** To the newest, when following; `force` follows again. */
	down = (force = false) => {
		if (force) this.following = true;
		if (!this.following || (this.touching && !force)) return;
		requestAnimationFrame(() => this.list?.scrollTo({ top: this.list.scrollHeight }));
	};
}
