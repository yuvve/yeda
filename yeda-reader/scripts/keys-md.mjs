import { readFileSync, writeFileSync, existsSync } from 'node:fs';

const source = readFileSync(new URL('../src/vim.ts', import.meta.url), 'utf8');
const start = source.indexOf('const table: Binding[] = [');
const block = source.slice(start, source.indexOf('\n\t];', start));

const entries = [];
let depth = 0;
let from = -1;
for (let i = block.indexOf('[') + 1; i < block.length; i++) {
	if (block[i] === '{' && depth++ === 0) from = i;
	else if (block[i] === '}' && --depth === 0) entries.push(block.slice(from, i + 1));
}

const string = (entry, name) => {
	const match = entry.match(new RegExp(`\\b${name}: ('(?:[^'\\\\]|\\\\.)*')`));
	return match && JSON.parse(`"${match[1].slice(1, -1).replace(/\\'/g, "'").replace(/"/g, '\\"')}"`);
};

const rows = entries.map((entry) => {
	const keys = entry.match(/keys: \[([^\]]*)\]/)?.[1].split(',').map(k => string(`x: ${k.trim()}`, 'x').replace(/^C-/, 'Ctrl-')) ?? [];
	const label = string(entry, 'label') ?? keys.join(keys.every(k => k.length === 1) ? '' : ' ');
	return `| \`${label}\` | ${string(entry, 'help')} |`;
});

const text = `# Reader keys\n\nGenerated from \`src/vim.ts\`; do not edit.\n\n| Keys | Does |\n| --- | --- |\n${rows.join('\n')}\n`;
const target = new URL('../KEYS.md', import.meta.url);
if (!existsSync(target) || readFileSync(target, 'utf8') !== text) writeFileSync(target, text);
