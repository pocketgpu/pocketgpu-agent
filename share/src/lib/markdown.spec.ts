import { describe, expect, it } from 'vitest';
import { inline, parse, safeUrl } from './markdown.js';

const text = (v: string) => ({ t: 'text', v });

describe('inline', () => {
	it('reads bold, italic, code and line breaks', () => {
		expect(inline('Keep **10%** aside, *then* run `train.py`\nnow')).toEqual([
			text('Keep '),
			{ t: 'b', c: [text('10%')] },
			text(' aside, '),
			{ t: 'i', c: [text('then')] },
			text(' run '),
			{ t: 'code', v: 'train.py' },
			{ t: 'br' },
			text('now')
		]);
	});

	it('leaves marks that are not closed, maths and snake_case as typed', () => {
		expect(inline('**half')).toEqual([text('**half')]);
		expect(inline('2 * 3 * 4')).toEqual([text('2 * 3 * 4')]);
		expect(inline('train_data_v2')).toEqual([text('train_data_v2')]);
	});

	it('opens only web and mail links', () => {
		expect(inline('[docs](https://ollama.com)')).toEqual([
			{ t: 'link', href: 'https://ollama.com', c: [text('docs')] }
		]);
		expect(inline('[x](javascript:alert(1))')).toEqual([text('x)')]);
		expect(inline('see https://pocketgpu.app.')).toEqual([
			text('see '),
			{ t: 'link', href: 'https://pocketgpu.app', c: [text('https://pocketgpu.app')] },
			text('.')
		]);
		expect(safeUrl('mailto:a@b.c')).toBe('mailto:a@b.c');
		expect(safeUrl('file:///etc/passwd')).toBeNull();
	});

	it('keeps HTML as text, but for a line break', () => {
		expect(inline('<script>alert(1)</script>')).toEqual([text('<script>alert(1)</script>')]);
		expect(inline('a<br>b')).toEqual([text('a'), { t: 'br' }, text('b')]);
	});

	it('reads bold italic and escaped marks', () => {
		expect(inline('***both***')).toEqual([{ t: 'b', c: [{ t: 'i', c: [text('both')] }] }]);
		expect(inline('\\*not italic\\*')).toEqual([text('*not italic*')]);
		expect(inline('C:\\Users\\me')).toEqual([text('C:\\Users\\me')]);
	});
});

describe('parse', () => {
	it('reads headings, paragraphs and rules', () => {
		expect(parse('# Title\nSome text\nmore\n\n---\n## Next')).toEqual([
			{ t: 'h', level: 1, c: [text('Title')] },
			{ t: 'p', c: [text('Some text'), { t: 'br' }, text('more')] },
			{ t: 'hr' },
			{ t: 'h', level: 2, c: [text('Next')] }
		]);
	});

	it('reads code blocks, also one still arriving', () => {
		expect(parse('```python\nimport torch\nprint(1)\n```\nDone')).toEqual([
			{ t: 'code', lang: 'python', v: 'import torch\nprint(1)' },
			{ t: 'p', c: [text('Done')] }
		]);
		expect(parse('Run:\n```bash\npgpu run -- python')).toEqual([
			{ t: 'p', c: [text('Run:')] },
			{ t: 'code', lang: 'bash', v: 'pgpu run -- python' }
		]);
	});

	it('reads lists as models write them, with a list inside', () => {
		const md =
			'*   **Linear**: simple\n*   **Neural**: deep\n    - layers\n    - more\n\n1. one\n2. two';
		expect(parse(md)).toEqual([
			{
				t: 'list',
				ordered: false,
				start: 1,
				items: [
					[{ t: 'p', c: [{ t: 'b', c: [text('Linear')] }, text(': simple')] }],
					[
						{ t: 'p', c: [{ t: 'b', c: [text('Neural')] }, text(': deep')] },
						{
							t: 'list',
							ordered: false,
							start: 1,
							items: [
								[{ t: 'p', c: [text('layers')] }],
								[{ t: 'p', c: [text('more')] }]
							]
						}
					]
				]
			},
			{
				t: 'list',
				ordered: true,
				start: 1,
				items: [[{ t: 'p', c: [text('one')] }], [{ t: 'p', c: [text('two')] }]]
			}
		]);
	});

	it('reads tables and quotes', () => {
		expect(parse('| Model | GPU |\n|---|---|\n| Small | 4 GB |\n\n> Tip: **wait**')).toEqual([
			{
				t: 'table',
				head: [[text('Model')], [text('GPU')]],
				rows: [[[text('Small')], [text('4 GB')]]]
			},
			{ t: 'quote', c: [{ t: 'p', c: [text('Tip: '), { t: 'b', c: [text('wait')] }] }] }
		]);
	});

	it('keeps one list across blank lines, and a plain line after an item in it', () => {
		expect(parse('1. a\n\n2. b\nmore\n\n3. c\n\nAfter')).toEqual([
			{
				t: 'list',
				ordered: true,
				start: 1,
				items: [
					[{ t: 'p', c: [text('a')] }],
					[{ t: 'p', c: [text('b'), { t: 'br' }, text('more')] }],
					[{ t: 'p', c: [text('c')] }]
				]
			},
			{ t: 'p', c: [text('After')] }
		]);
	});

	it('reads code inside a list item, and a fence with a language inside code', () => {
		expect(parse('1. Install:\n   ```bash\n   pip install torch\n   ```\n2. Run')).toEqual([
			{
				t: 'list',
				ordered: true,
				start: 1,
				items: [
					[
						{ t: 'p', c: [text('Install:')] },
						{ t: 'code', lang: 'bash', v: 'pip install torch' }
					],
					[{ t: 'p', c: [text('Run')] }]
				]
			}
		]);
		expect(parse('```md\n```js\nx\n```')).toEqual([{ t: 'code', lang: 'md', v: '```js\nx' }]);
	});

	it('keeps a heading that ends in #, and a table not yet complete as text', () => {
		expect(parse('## Using C#')).toEqual([{ t: 'h', level: 2, c: [text('Using C#')] }]);
		expect(parse('| a | b |')).toEqual([{ t: 'p', c: [text('| a | b |')] }]);
		expect(parse('| a | b<br>c |\n|:-|-:|')).toEqual([
			{ t: 'table', head: [[text('a')], [text('b'), { t: 'br' }, text('c')]], rows: [] }
		]);
	});

	it('reads a line with === or --- under it as a heading, and === on its own as a rule', () => {
		expect(parse('**Step 2: Prepare**\n-----------')).toEqual([
			{ t: 'h', level: 2, c: [{ t: 'b', c: [text('Step 2: Prepare')] }] }
		]);
		expect(parse('Title\n===\n\nText')).toEqual([
			{ t: 'h', level: 1, c: [text('Title')] },
			{ t: 'p', c: [text('Text')] }
		]);
		expect(parse('# Title\n=========\nText\nmore\n=====')).toEqual([
			{ t: 'h', level: 1, c: [text('Title')] },
			{ t: 'hr' },
			{ t: 'p', c: [text('Text'), { t: 'br' }, text('more')] },
			{ t: 'hr' }
		]);
	});
});
