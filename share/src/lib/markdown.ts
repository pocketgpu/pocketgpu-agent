// Markdown in a model's answer, read into blocks and pieces of text (board 80) that the chat draws
// with its own elements: headings, paragraphs, lists, code, tables, quotes and lines. Nothing is
// turned into HTML, so nothing an answer says can run; a link opens only for http, https or mail.
// An answer still arriving may end inside a code block or before a closing `**`: the code shows
// as code so far, and an unclosed mark shows as typed. It is read again with each new piece, so
// reading stays one pass over the text.

export type Inline =
	| { t: 'text'; v: string }
	| { t: 'br' }
	| { t: 'b'; c: Inline[] }
	| { t: 'i'; c: Inline[] }
	| { t: 'code'; v: string }
	| { t: 'link'; href: string; c: Inline[] };

export type Block =
	| { t: 'h'; level: number; c: Inline[] }
	| { t: 'p'; c: Inline[] }
	| { t: 'code'; lang: string; v: string }
	| { t: 'list'; ordered: boolean; start: number; items: Block[][] }
	| { t: 'quote'; c: Block[] }
	| { t: 'table'; head: Inline[][]; rows: Inline[][][] }
	| { t: 'hr' };

const FENCE = /^\s*(```+|~~~+)\s*([\w+#.-]*)[^`]*$/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^\s{0,3}>\s?(.*)$/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const DIVIDER = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
// Under a line of text, === or --- makes it a heading; a line of === on its own is a rule.
const UNDERLINE = /^\s{0,3}(={2,}|-{2,})\s*$/;
const BAR = /^\s{0,3}={3,}\s*$/;

/** An answer's blocks. */
export function parse(src: string): Block[] {
	return blocks(src.replace(/\r\n?/g, '\n').split('\n'));
}

const indentOf = (line: string) => line.length - line.trimStart().length;
/** A list item's parts: indent, marker and text; a line of `* * *` is a rule, not an item. */
const item = (line: string | undefined) =>
	line == null || RULE.test(line) ? null : ITEM.exec(line);
const isTable = (line: string, next: string | undefined) =>
	line.includes('|') && next != null && next.includes('|') && DIVIDER.test(next);
const startsBlock = (line: string, next: string | undefined) =>
	FENCE.test(line) ||
	HEADING.test(line) ||
	RULE.test(line) ||
	BAR.test(line) ||
	QUOTE.test(line) ||
	ITEM.test(line) ||
	isTable(line, next);

function blocks(lines: string[]): Block[] {
	const out: Block[] = [];
	let i = 0;
	while (i < lines.length) {
		const line = lines[i];
		if (!line.trim()) {
			i++;
			continue;
		}
		const fence = FENCE.exec(line);
		if (fence) {
			const mark = fence[1];
			const close = lines.findIndex(
				(l, j) => j > i && l.trim().startsWith(mark) && /^(`+|~+)$/.test(l.trim())
			);
			const end = close < 0 ? lines.length : close;
			const pad = indentOf(line);
			const body = lines.slice(i + 1, end).map((l) => l.slice(Math.min(pad, indentOf(l))));
			out.push({ t: 'code', lang: fence[2], v: body.join('\n') });
			i = end + 1;
			continue;
		}
		const h = HEADING.exec(line);
		if (h) {
			out.push({ t: 'h', level: h[1].length, c: inline(h[2]) });
			i++;
			continue;
		}
		if (RULE.test(line) || BAR.test(line)) {
			out.push({ t: 'hr' });
			i++;
			continue;
		}
		if (isTable(line, lines[i + 1])) {
			const head = cells(line).map((c) => inline(c));
			const rows: Inline[][][] = [];
			for (i += 2; i < lines.length && lines[i].includes('|') && lines[i].trim(); i++) {
				const row = cells(lines[i]).map((c) => inline(c));
				while (row.length < head.length) row.push([]);
				rows.push(row);
			}
			out.push({ t: 'table', head, rows });
			continue;
		}
		if (QUOTE.test(line)) {
			const inner: string[] = [];
			for (let q; i < lines.length && (q = QUOTE.exec(lines[i])); i++) inner.push(q[1]);
			out.push({ t: 'quote', c: blocks(inner) });
			continue;
		}
		const first = item(line);
		if (first) {
			i = list(lines, i, first, out);
			continue;
		}
		const under = UNDERLINE.exec(lines[i + 1] ?? '');
		if (under) {
			out.push({ t: 'h', level: under[1].startsWith('=') ? 1 : 2, c: inline(line.trim()) });
			i += 2;
			continue;
		}
		const para = [line.trim()];
		for (i++; i < lines.length && lines[i].trim() && !startsBlock(lines[i], lines[i + 1]); i++)
			para.push(lines[i].trim());
		out.push({ t: 'p', c: inline(para.join('\n')) });
	}
	return out;
}

/** The list that starts at line `i`; returns where it ends. Lines indented under an item belong
 * to it (more text, or a list inside), and so does a plain line right after its text. Blank lines
 * between items keep one list. */
function list(lines: string[], i: number, first: RegExpExecArray, out: Block[]): number {
	const base = first[1].length;
	const ordered = /\d/.test(first[2]);
	const items: Block[][] = [];
	for (
		let m: RegExpExecArray | null = first;
		m && m[1].length === base && /\d/.test(m[2]) === ordered;
		m = item(lines[i])
	) {
		// Where the item's text starts: lines under it are read from there.
		const at = m[0].length - m[3].length;
		const own = [m[3]];
		let blank = false;
		for (i++; i < lines.length; i++) {
			const l = lines[i];
			if (!l.trim()) {
				blank = true;
				own.push('');
				continue;
			}
			const other = item(l);
			if (other && other[1].length <= base) break;
			if (indentOf(l) > base) own.push(l.slice(Math.min(indentOf(l), at)));
			else if (!blank && !startsBlock(l, lines[i + 1])) own.push(l.trim());
			else break;
			blank = false;
		}
		// Blank lines before the next item or the text after the list aren't the item's.
		while (own.length > 1 && !own[own.length - 1].trim()) own.pop();
		items.push(blocks(own));
	}
	out.push({ t: 'list', ordered, start: ordered ? parseInt(first[2], 10) : 1, items });
	return i;
}

/** A table row's cells, without the outer pipes. */
function cells(line: string): string[] {
	let s = line.trim();
	if (s.startsWith('|')) s = s.slice(1);
	if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
	return s.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, '|'));
}

/** A link's address when it may open: web or mail only. */
export function safeUrl(href: string): string | null {
	return /^(https?:\/\/|mailto:)/i.test(href.trim()) ? href.trim() : null;
}

// The marks inside a line, tried in order where the line has the character they start with
// (`edge`: only at the start of a word). Each gives a piece, or plain text.
interface Mark {
	on: string;
	edge?: true;
	re: RegExp;
	make: (m: RegExpExecArray) => Inline | string;
}
const MARKS: Mark[] = [
	{ on: '\n', re: /\n/y, make: () => ({ t: 'br' }) },
	{ on: '\\', re: /\\([!-/:-@[-`{-~])/y, make: (m) => m[1] },
	{
		on: '`',
		re: /(`+)(?!`)([\s\S]*?[^`])\1(?!`)/y,
		make: (m) => ({ t: 'code', v: m[2].replace(/^ ([\s\S]+) $/, '$1') })
	},
	// Backticks that open nothing stay as typed, all of them.
	{ on: '`', re: /`+/y, make: (m) => m[0] },
	{
		on: '*',
		re: /\*\*\*(?=\S)([\s\S]*?\S)\*\*\*(?!\*)/y,
		make: (m) => ({ t: 'b', c: [{ t: 'i', c: inline(m[1]) }] })
	},
	{ on: '*', re: /\*\*(?=\S)([\s\S]*?\S)\*\*/y, make: (m) => ({ t: 'b', c: inline(m[1]) }) },
	{
		on: '_',
		edge: true,
		re: /__(?=\S)([\s\S]*?\S)__(?![\p{L}\p{N}])/uy,
		make: (m) => ({ t: 'b', c: inline(m[1]) })
	},
	{
		on: '*',
		re: /\*(?=[^\s*])([^*\n]*?[^\s*])\*(?!\*)/y,
		make: (m) => ({ t: 'i', c: inline(m[1]) })
	},
	{
		on: '_',
		edge: true,
		re: /_(?=\S)([^_\n]*?\S)_(?![\p{L}\p{N}])/uy,
		make: (m) => ({ t: 'i', c: inline(m[1]) })
	},
	{
		on: '[',
		re: /\[([^\]\n]+)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)/y,
		make: (m) => {
			const href = safeUrl(m[2]);
			return href ? { t: 'link', href, c: inline(m[1]) } : m[1];
		}
	},
	{
		on: 'h',
		edge: true,
		re: /https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"*_]/y,
		make: (m) => ({ t: 'link', href: m[0], c: [{ t: 'text', v: m[0] }] })
	},
	{ on: '<', re: /<br\s*\/?>/iy, make: () => ({ t: 'br' }) }
];
const WORD = /[\p{L}\p{N}]/u;

/** A line's bold, italic, code, links and line breaks. */
export function inline(src: string): Inline[] {
	const out: Inline[] = [];
	let text = '';
	let i = 0;
	scan: while (i < src.length) {
		const edge = i === 0 || !WORD.test(src[i - 1]);
		for (const mark of MARKS) {
			if (mark.on !== src[i] || (mark.edge && !edge)) continue;
			mark.re.lastIndex = i;
			const m = mark.re.exec(src);
			if (!m) continue;
			const made = mark.make(m);
			if (typeof made === 'string') text += made;
			else {
				if (text) out.push({ t: 'text', v: text });
				text = '';
				out.push(made);
			}
			i += m[0].length;
			continue scan;
		}
		text += src[i];
		i++;
	}
	if (text) out.push({ t: 'text', v: text });
	return out;
}
