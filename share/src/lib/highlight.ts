// Colours for a code block in a model's answer (board 82): keywords, quoted text, numbers and
// options, function and command names, comments. Python, shell, JavaScript, TypeScript and JSON;
// other languages stay one colour. The code is read again as it arrives, so this is one pass,
// and a string or comment that isn't closed yet runs to the end. The pieces put together are
// the code exactly as written.

export type Tone = 'kw' | 'str' | 'num' | 'fn' | 'com';
export interface Piece {
	v: string;
	tone?: Tone;
}

/** A piece of code and its colour; `null` leaves it plain. Tried in order at each place. */
type Rule = [Tone | null, RegExp];

const NAMES: Record<string, 'python' | 'shell' | 'js' | 'ts' | 'json'> = {
	py: 'python',
	python: 'python',
	python3: 'python',
	sh: 'shell',
	bash: 'shell',
	zsh: 'shell',
	shell: 'shell',
	console: 'shell',
	'shell-session': 'shell',
	js: 'js',
	javascript: 'js',
	jsx: 'js',
	mjs: 'js',
	cjs: 'js',
	ts: 'ts',
	typescript: 'ts',
	tsx: 'ts',
	json: 'json',
	jsonc: 'json'
};

const PYTHON: Rule[] = [
	['com', /#[^\n]*/y],
	['str', /[rRbBuUfF]{0,2}("""|''')[\s\S]*?(?:\1|$)/y],
	['str', /[rRbBuUfF]{0,2}(?:"(?:[^"\\\n]|\\.)*"?|'(?:[^'\\\n]|\\.)*'?)/y],
	['fn', /@[\w.]+/y],
	['num', /\b(?:True|False|None)\b/y],
	['num', /\b(?:0[xXoObB][\da-fA-F_]+|\d[\d_]*(?:\.\d*)?(?:[eE][+-]?\d+)?[jJ]?)/y],
	[
		'kw',
		/\b(?:and|as|assert|async|await|break|class|continue|def|del|elif|else|except|finally|for|from|global|if|import|in|is|lambda|nonlocal|not|or|pass|raise|return|try|while|with|yield)\b/y
	],
	['fn', /[A-Za-z_]\w*(?=\s*\()/y],
	[null, /[A-Za-z_]\w*/y]
];

const JS_WORDS =
	'async|await|break|case|catch|class|const|continue|debugger|default|delete|do|else|export|extends|finally|for|from|function|if|import|in|instanceof|let|new|of|return|static|super|switch|this|throw|try|typeof|var|void|while|with|yield';
const TS_WORDS = `${JS_WORDS}|abstract|as|declare|enum|implements|interface|keyof|namespace|private|protected|public|readonly|satisfies|type`;
const script = (words: string): Rule[] => [
	['com', /\/\/[^\n]*/y],
	['com', /\/\*[\s\S]*?(?:\*\/|$)/y],
	['str', /"(?:[^"\\\n]|\\.)*"?|'(?:[^'\\\n]|\\.)*'?/y],
	['str', /`(?:[^`\\]|\\[\s\S])*`?/y],
	['num', /\b(?:true|false|null|undefined|NaN|Infinity)\b/y],
	['num', /\b(?:0[xXbBoO][\da-fA-F_]+n?|\d[\d_]*(?:\.\d*)?(?:[eE][+-]?\d+)?n?)/y],
	['kw', new RegExp(`\\b(?:${words})\\b`, 'y')],
	['fn', /[A-Za-z_$][\w$]*(?=\s*\()/y],
	[null, /[A-Za-z_$][\w$]*/y]
];

const JSON_RULES: Rule[] = [
	['com', /\/\/[^\n]*/y],
	['fn', /"(?:[^"\\\n]|\\.)*"(?=\s*:)/y],
	['str', /"(?:[^"\\\n]|\\.)*"?/y],
	['num', /\b(?:true|false|null)\b|-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/y]
];

const RULES = { python: PYTHON, js: script(JS_WORDS), ts: script(TS_WORDS), json: JSON_RULES };

/** A language for a block whose model didn't name one, from how the code looks: `python`,
 * `javascript`, `json` or `shell`, or '' when nothing fits (the block stays plain). Small
 * models often leave the language out. */
export function guess(code: string): string {
	const c = code.trim();
	if (!c) return '';
	if (/^[[{]/.test(c)) {
		try {
			JSON.parse(c);
			return 'json';
		} catch {
			/* not JSON */
		}
	}
	if (/^(?:def|class) \w+.*:\s*$|^(?:from \S+ )?import \w|^\s*print\(|^if __name__/m.test(c))
		return 'python';
	if (/^\s*(?:const|let|var|function|export|import .+ from) |=>|console\.log\(/m.test(c))
		return 'javascript';
	if (
		/^\s*(?:\$ |sudo |npm |npx |pip3? |cd |ls |git |curl |brew |apt(?:-get)? |docker |ollama )/m.test(
			c
		)
	)
		return 'shell';
	return '';
}

/** The code in coloured pieces; plain in one piece for a language not known here. */
export function highlight(code: string, lang: string): Piece[] {
	const name = NAMES[lang.trim().toLowerCase()];
	if (!name) return [{ v: code }];
	return name === 'shell' ? shell(code) : run(code, RULES[name]);
}

/** Pieces, with neighbours of one colour joined. */
class Pieces {
	list: Piece[] = [];
	add(v: string, tone?: Tone | null) {
		const last = this.list[this.list.length - 1];
		if (last && last.tone === (tone ?? undefined)) last.v += v;
		else this.list.push(tone ? { v, tone } : { v });
	}
}

function run(code: string, rules: Rule[]): Piece[] {
	const out = new Pieces();
	let i = 0;
	scan: while (i < code.length) {
		for (const [tone, re] of rules) {
			re.lastIndex = i;
			const m = re.exec(code);
			if (m?.[0]) {
				out.add(m[0], tone);
				i += m[0].length;
				continue scan;
			}
		}
		out.add(code[i]);
		i++;
	}
	return out.list;
}

const SHELL: Rule[] = [
	['str', /"(?:[^"\\]|\\[\s\S])*"?/y],
	['str', /'[^']*'?/y],
	['num', /\$\{[^}\n]*\}?|\$[A-Za-z_]\w*|\$[@#?$!*\d-]/y],
	[
		'kw',
		/(?:if|then|else|elif|fi|for|while|until|do|done|case|esac|in|function|select|return|export|local|readonly)(?![^\s;|&()<>])/y
	]
];
// Keywords and commands whose next word is a command too.
const LEADS = new Set(['if', 'then', 'else', 'elif', 'while', 'until', 'do']);
const RUNNERS = new Set(['sudo', 'env', 'time', 'nohup', 'exec', 'xargs', 'watch']);
const WORD = /[^\s;|&()<>"'`$]+/y;

/** Shell: the first word of a command is its name (also after `|`, `;`, `&&` or `$(`), `-x`
 * and `--name` are options, `#` starts a comment and `$ ` at the start of a line is a prompt. */
function shell(code: string): Piece[] {
	const out = new Pieces();
	let i = 0;
	let command = true;
	scan: while (i < code.length) {
		const c = code[i];
		const lineStart = i === 0 || code[i - 1] === '\n';
		if (c === '\n') {
			// A line ending in \ goes on with the same command.
			command = code[i - 1] !== '\\';
			out.add(c);
			i++;
			continue;
		}
		if (lineStart && code.startsWith('$ ', i)) {
			out.add('$ ', 'com');
			i += 2;
			command = true;
			continue;
		}
		if (c === '#' && (i === 0 || /[\s;|&(]/.test(code[i - 1]))) {
			const end = code.indexOf('\n', i);
			const v = end < 0 ? code.slice(i) : code.slice(i, end);
			out.add(v, 'com');
			i += v.length;
			continue;
		}
		if (/[;|&(]/.test(c)) {
			out.add(c);
			i++;
			command = true;
			continue;
		}
		if (/\s/.test(c)) {
			out.add(c);
			i++;
			continue;
		}
		for (const [tone, re] of SHELL) {
			re.lastIndex = i;
			const m = re.exec(code);
			if (m?.[0]) {
				out.add(m[0], tone);
				i += m[0].length;
				command = tone === 'kw' && LEADS.has(m[0]);
				continue scan;
			}
		}
		WORD.lastIndex = i;
		const word = WORD.exec(code)?.[0];
		if (!word) {
			out.add(c);
			i++;
			continue;
		}
		if (/^--?\w/.test(word) && /\s/.test(code[i - 1] ?? ' ')) out.add(word, 'num');
		else if (command && !word.includes('=') && !word.startsWith('[')) out.add(word, 'fn');
		else out.add(word, /^\d+$/.test(word) ? 'num' : null);
		// `FOO=1 cmd`: the command still comes; `sudo cmd`: so does this one.
		command = (command && word.includes('=')) || RUNNERS.has(word);
		i += word.length;
	}
	return out.list;
}
