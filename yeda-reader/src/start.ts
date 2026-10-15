import { invoke } from '@tauri-apps/api/core';

/** Implemented by `MainActivity.Bridge`; async results arrive by calling `window[callback]`. */
export interface AndroidBridge {
	hasFileAccess(): boolean;
	requestFileAccess(callback: string): void;
	pickFolder(callback: string): void;
	takeOpenWithPath(): string | null;
}

interface RefSummary {
	key: string;
	title: string;
	authors: string[];
	year: number | null;
	has_pdf: boolean;
}

type OpenRef = { status: 'opened' } | { status: 'missingLocation'; location: string };

declare global {
	interface Window {
		YedaAndroid?: AndroidBridge;
		[callback: `yeda${string}`]: ((value: unknown) => void) | undefined;
	}
}

let callbacks = 0;

function bridgeCall<T>(call: (callback: string) => void): Promise<T> {
	const name = `yeda${callbacks++}` as const;
	return new Promise((resolve) => {
		window[name] = (value) => {
			delete window[name];
			resolve(value as T);
		};
		call(name);
	});
}

const STYLE = `
.start { position: fixed; inset: 0; z-index: 10000; overflow: auto; background: #fff; color: #000;
	font: 18px sans-serif; padding: 16px; box-sizing: border-box; }
.start button, .start input { font: inherit; padding: 12px 16px; border: 2px solid #000; background: #fff; color: #000; }
.start input { width: 100%; box-sizing: border-box; margin: 12px 0; }
.start .bar { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
.start .path { flex: 1; overflow-wrap: anywhere; font-size: 14px; }
.start ul { list-style: none; margin: 0; padding: 0; }
.start li { padding: 12px 4px; border-bottom: 1px solid #000; }
.start li.no-pdf { opacity: 0.5; }
.start .meta { font-size: 14px; margin-top: 4px; }
.start .error { border: 2px solid #000; padding: 12px; margin: 12px 0; }
`;

/** Shows the library picker and reference list; resolves once a document is open. */
export function showStartScreen(android: AndroidBridge): Promise<void> {
	const style = document.createElement('style');
	style.textContent = STYLE;
	const root = document.createElement('div');
	root.className = 'start';
	document.body.append(style, root);

	return new Promise((resolve) => {
		const el = <K extends keyof HTMLElementTagNameMap>(tag: K, text = '', className = '') => {
			const e = document.createElement(tag);
			e.textContent = text;
			e.className = className;
			return e;
		};
		const button = (text: string, onClick: () => void) => {
			const b = el('button', text);
			b.addEventListener('click', onClick);
			return b;
		};
		const fail = (e: unknown) => root.prepend(el('div', String(e), 'error'));

		const pickLibrary = async () => {
			const path = await bridgeCall<string | null>(cb => android.pickFolder(cb));
			if (path) await invoke('set_library', { path }).then(render, fail);
		};

		const open = async (key: string) => {
			const result = await invoke<OpenRef>('open_ref', { key });
			if (result.status === 'opened') {
				root.remove();
				style.remove();
				resolve();
				return;
			}
			if (!window.confirm(`Folder for location "${result.location}" not found on this device. Pick it now?`)) return;
			const path = await bridgeCall<string | null>(cb => android.pickFolder(cb));
			if (path) {
				await invoke('set_location', { name: result.location, path });
				await open(key);
			}
		};

		const render = async () => {
			root.replaceChildren();
			if (!android.hasFileAccess()) {
				root.append(
					el('p', 'yeda-reader needs access to all files to read and write the library folder.'),
					button('Grant file access', () => bridgeCall(cb => android.requestFileAccess(cb)).then(render)),
				);
				return;
			}
			const library = await invoke<string | null>('library');
			const bar = el('div', '', 'bar');
			bar.append(el('span', library ?? 'No library folder chosen', 'path'), button('Choose library', pickLibrary));
			root.append(bar);
			if (!library) return;

			const refs = await invoke<RefSummary[]>('list_refs').catch((e) => {
				fail(e);
				return [];
			});
			const search = el('input');
			search.placeholder = `Search ${refs.length} references`;
			const list = el('ul');
			const show = () => {
				const words = search.value.toLowerCase().split(/\s+/).filter(Boolean);
				list.replaceChildren(...refs
					.filter((r) => {
						const haystack = `${r.key} ${r.title} ${r.authors.join(' ')} ${r.year ?? ''}`.toLowerCase();
						return words.every(w => haystack.includes(w));
					})
					.map((r) => {
						const li = el('li', r.title || r.key, r.has_pdf ? '' : 'no-pdf');
						li.append(el('div', [r.authors.join(', '), r.year].filter(Boolean).join(' · '), 'meta'));
						if (r.has_pdf) li.addEventListener('click', () => open(r.key).catch(fail));
						return li;
					}));
			};
			search.addEventListener('input', show);
			root.append(search, list);
			show();
		};

		render().catch(fail);
	});
}
