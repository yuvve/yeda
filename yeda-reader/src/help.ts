import { Entry, JumpReader, showPopup } from './jump';
import type { Binding, VimReader } from './vim';

function keysOf(binding: Binding): string {
	if (binding.label) return binding.label;
	const keys = (binding.keys ?? []).map(key => key.replace(/^C-/, 'Ctrl-'));
	return keys.join(keys.every(key => key.length === 1) ? '' : ' ');
}

export function showHelp(reader: VimReader & JumpReader, bindings: Binding[]) {
	const entries: Entry[] = bindings.map((binding) => {
		const keys = keysOf(binding);
		return {
			label: binding.help,
			search: `${binding.help} ${keys}`,
			detail: keys,
			indent: 0,
			go: () => {
				if (binding.run) binding.run(reader, false, 1);
			},
		};
	});
	showPopup(reader, 'Search keybinds', entries, 'No keybinds');
}
