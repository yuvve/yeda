import { invoke } from '@tauri-apps/api/core';
import { hasFileAccess, pickFolder, pickPdf, requestFileAccess } from './bridge';
import { inspect, plainText, Region } from './pdftext';

type Filter =
	| { kind: 'all' }
	| { kind: 'search'; value: [string, string] }
	| { kind: 'collection'; value: string }
	| { kind: 'tag'; value: string };

interface Overview {
	sidebar: { filter: Filter; color: string | null }[];
	statuses: string[];
	tables: string[];
	filter_fields: [string, string][];
	sort_fields: [string, string][];
	locations: string[];
}

interface Sort {
	field: string;
	descending: boolean;
}

interface Row {
	key: string;
	title: string;
	authors: string[];
	year: number | null;
	status: string | null;
	tags: string[];
	has_pdf: boolean;
}

interface Annotation {
	id: string;
	kind: string;
	page: string;
	color?: string;
	text?: string;
	comment?: string;
	image?: string;
}

interface Ref {
	type: string;
	title: string;
	authors: string[];
	year?: number;
	doi?: string;
	tags: string[];
	collections: string[];
	status?: string;
	pdf?: { location: string; path: string };
	extra?: Record<string, unknown>;
	tables?: Record<string, Record<string, string>>;
}

interface Details {
	key: string;
	ref: Ref;
	raw: string;
	note: string;
	annotations: Annotation[];
	tables: Record<string, string[]>;
}

type OpenRef =
	| { status: 'opened' }
	| { status: 'missingLocation'; location: string }
	| { status: 'needsPassword'; account: string; label: string; rejected: boolean };

const STYLE = `
.yeda { position: fixed; inset: 0; z-index: 10000; display: flex; flex-direction: column; background: #fff; color: #000;
	font: 18px sans-serif; }
.yeda * { box-sizing: border-box; transition: none !important; animation: none !important; }
.yeda button, .yeda input, .yeda select, .yeda textarea { font: inherit; padding: 10px 14px; border: 2px solid #000;
	background: #fff; color: #000; border-radius: 0; }
.yeda .bar { display: flex; gap: 8px; padding: 8px; border-bottom: 2px solid #000; align-items: center; flex-wrap: wrap; }
.yeda .bar input { flex: 1; min-width: 8em; }
.yeda main { flex: 1; overflow: auto; padding: 8px; }
.yeda .status { padding: 8px; border-top: 2px solid #000; min-height: 1.5em; font-size: 14px; overflow-wrap: anywhere; }
.yeda .status:empty { display: none; }
.yeda ul { list-style: none; margin: 0; padding: 0; }
.yeda li { padding: 12px 4px; border-bottom: 1px solid #000; }
.yeda .meta { font-size: 14px; margin-top: 4px; }
.yeda .chip { display: inline-block; border: 1px solid #000; padding: 2px 8px; margin: 2px 4px 2px 0; font-size: 14px; }
.yeda .chip button { border: none; padding: 0 0 0 8px; background: none; color: inherit; }
.yeda .actions { display: flex; flex-wrap: wrap; gap: 8px; margin: 12px 0; }
.yeda table { border-collapse: collapse; width: 100%; font-size: 15px; }
.yeda td { border-bottom: 1px solid #000; padding: 6px 4px; vertical-align: top; overflow-wrap: anywhere; }
.yeda td:first-child { font-weight: bold; width: 30%; }
.yeda textarea { width: 100%; height: 60vh; }
.yeda textarea.mono { font-family: monospace; font-size: 15px; }
.yeda .annotation { border-left: 8px solid #000; padding-left: 8px; }
.yeda .annotation img { max-width: 100%; border: 1px solid #000; }
.yeda .dim { opacity: 0.5; }
.yeda h2 { font-size: 20px; margin: 8px 0; }
`;

const GLYPHS: Record<string, string> = { 'New': '✦', 'To Read': '○', 'In Progress': '◐', 'Read': '●', 'Not Reading': '✕' };

type Child = Node | string | null | undefined | false;

function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Record<string, unknown> = {}, ...children: Child[]) {
	const e = document.createElement(tag);
	Object.assign(e, props);
	e.append(...children.filter((c): c is Node | string => !!c));
	return e;
}

const button = (text: string, onclick: () => unknown) => h('button', { onclick }, text);

function filterLabel(f: Filter): string {
	switch (f.kind) {
		case 'all': return 'All';
		case 'search': return `⌕ ${f.value[0]}`;
		case 'collection': return `${'  '.repeat(f.value.split('/').length - 1)}${f.value.split('/').pop()}`;
		case 'tag': return `#${f.value}`;
	}
}

/** Shows the library browser; `openReader` switches to the reader once a PDF is open. */
/** Returns the Back handler: true if it went back to the list, false if already there. */
export function showBrowser(openReader: () => void): () => boolean {
	const style = h('style', { textContent: STYLE });
	const status = h('div', { className: 'status' });
	const main = h('main');
	const search = h('input', { type: 'search', placeholder: 'Search (author:smith year:2020 …)' });
	const filterButton = h('button');
	const sortButton = h('button', { onclick: () => showSortMenu() });
	const root = h('div', { className: 'yeda' },
		h('div', { className: 'bar' }, filterButton, search, button('Filter', () => run(showFilterForm)),
			sortButton, button('+', () => addReference()), button('⚙', () => showSettings())),
		main, status);
	document.body.append(style, root);

	let overview: Overview = { sidebar: [], statuses: [], tables: [], filter_fields: [], sort_fields: [], locations: [] };
	let filter: Filter = { kind: 'all' };
	let sort: Sort = { field: 'key', descending: false };

	const say = (text: string) => {
		status.textContent = text;
	};
	/** Runs `action`, showing its error instead of throwing. */
	const run = async <T>(action: () => Promise<T>): Promise<T | undefined> => {
		try {
			return await action();
		} catch (e) {
			say(String(e));
		}
	};
	let atList = false;
	const show = (...children: Child[]) => {
		atList = false;
		main.replaceChildren(...children.filter((c): c is Node | string => !!c));
		main.scrollTop = 0;
	};
	const tagColor = (tag: string) => overview.sidebar.find(e => e.filter.kind === 'tag' && e.filter.value === tag)?.color ?? null;
	const chip = (tag: string) => {
		const c = h('span', { className: 'chip' }, tag);
		const color = tagColor(tag);
		if (color) c.style.borderColor = c.style.color = color;
		return c;
	};
	const all = (kind: 'tag' | 'collection') =>
		overview.sidebar.flatMap(e => (e.filter.kind === kind ? [e.filter.value] : []));
	const datalist = (id: string, values: string[]) => h('datalist', { id }, ...values.map(v => h('option', { value: v })));

	async function refresh() {
		overview = await invoke<Overview>('refresh');
		filterButton.textContent = `${filterLabel(filter).trim()} ▾`;
		filterButton.onclick = showSidebar;
		sortButton.textContent = `${sort.descending ? '▼' : '▲'} ${overview.sort_fields.find(([f]) => f === sort.field)?.[1] ?? sort.field}`;
	}

	/** Enter on the current field reverses the order. */
	function showSortMenu() {
		show(h('ul', {}, ...overview.sort_fields.map(([field, label]) => h('li', {
			onclick: () => run(async () => {
				sort = { field, descending: field === sort.field && !sort.descending };
				await refresh();
				await showList();
			}),
		}, `${field === sort.field ? (sort.descending ? '▼ ' : '▲ ') : ''}${label}`))));
	}

	/** A form view of the search query: one input per field, then free text. */
	async function showFilterForm() {
		const parsed = await invoke<{ fields: [string, string][]; free: string }>('parse_query', { query: search.value });
		const suggestions = await invoke<Record<string, string[]>>('filter_suggestions');
		const known = new Set(overview.filter_fields.map(([f]) => f));
		const kept = parsed.fields.filter(([f]) => !known.has(f));
		const inputs = overview.filter_fields.map(([field, label]) => {
			const input = h('input', { value: parsed.fields.find(([f]) => f === field)?.[1] ?? '' });
			input.setAttribute('list', `yeda-${field}`);
			return [field, label, input] as const;
		});
		const free = h('input', { value: parsed.free });
		const apply = () => run(async () => {
			const fields = [...kept, ...inputs.filter(([, , i]) => i.value.trim()).map(([f, , i]) => [f, i.value.trim()])];
			search.value = await invoke<string>('compose_query', { fields, free: free.value });
			await showList();
		});
		show(h('h2', {}, 'Filter'),
			h('table', {}, ...inputs.map(([, label, input]) => h('tr', {}, h('td', {}, label), h('td', {}, input))),
				h('tr', {}, h('td', {}, 'Text'), h('td', {}, free))),
			...overview.filter_fields.map(([field]) => datalist(`yeda-${field}`, suggestions[field] ?? [])),
			h('div', { className: 'actions' }, button('Apply', apply), button('Cancel', () => run(showList))));
	}

	async function showList() {
		const rows = await invoke<Row[]>('list', { filter, query: search.value, sort });
		const list = h('ul', {}, ...rows.map(r => h('li', { onclick: () => run(() => showDetails(r.key)) },
			h('div', {}, `${GLYPHS[r.status ?? ''] ?? '·'} `, r.title || r.key),
			h('div', { className: 'meta' }, [r.authors[0]?.split(',')[0], r.year].filter(Boolean).join(' · '),
				r.has_pdf ? '' : ' · no PDF'),
			r.tags.length > 0 && h('div', {}, ...r.tags.map(chip)))));
		const saveSearch = search.value.trim() && button('Save this search', () => run(async () => {
			const name = window.prompt('Name for this search');
			if (!name) return;
			await invoke('save_search', { name, query: search.value });
			filter = { kind: 'search', value: [name, search.value] };
			search.value = '';
			await refresh();
			await showList();
		}));
		show(h('div', { className: 'meta' }, `${rows.length} references`), saveSearch, list);
		atList = true;
	}

	function showSidebar() {
		const entries = overview.sidebar.map(({ filter: f, color }) => {
			const label = h('span', { style: color ? `color: ${color}` : '' }, filterLabel(f));
			const pick = () => run(async () => {
				filter = f;
				await refresh();
				await showList();
			});
			const extra = f.kind === 'tag'
				? button('colour', () => run(async () => {
					const c = window.prompt(`Colour for #${f.value} (#rrggbb or a name; empty removes)`, color ?? '');
					if (c === null) return;
					await invoke('set_tag_color', { tag: f.value, color: c });
					await refresh();
					showSidebar();
				}))
				: f.kind === 'search' && button('delete', () => run(async () => {
					if (!window.confirm(`Delete saved search "${f.value[0]}"?`)) return;
					await invoke('delete_search', { name: f.value[0] });
					filter = { kind: 'all' };
					await refresh();
					showSidebar();
				}));
			const li = h('li', {}, label, ' ', extra || null);
			label.onclick = pick;
			label.style.whiteSpace = 'pre';
			return li;
		});
		show(h('ul', {}, ...entries));
	}

	async function showDetails(key: string) {
		const d = await invoke<Details>('details', { key });
		const r = d.ref;
		const back = button('← Back', () => run(showList));
		const reload = () => run(async () => {
			await refresh();
			await showDetails(key);
		});

		const statusSelect = h('select', {}, h('option', { value: '' }, 'No status'),
			...overview.statuses.map(s => h('option', { value: s, selected: s === r.status }, `${GLYPHS[s]} ${s}`)));
		statusSelect.onchange = () => run(async () => {
			await invoke('set_status', { key, status: statusSelect.value || null });
			await reload();
		});

		const setTags = (tags: string[]) => run(async () => {
			await invoke('set_tags', { key, tags });
			await reload();
		});
		const tagInput = h('input', { placeholder: 'Add tag' });
		tagInput.setAttribute('list', 'yeda-tags');
		const tags = h('div', {}, ...r.tags.map((t) => {
			const c = chip(t);
			c.append(' ', h('button', { onclick: () => setTags(r.tags.filter(x => x !== t)) }, '×'));
			return c;
		}), tagInput, button('Add', () => tagInput.value.trim() && setTags([...r.tags, tagInput.value.trim()])),
		datalist('yeda-tags', all('tag')));

		const collInput = h('input', { placeholder: 'Collection', value: r.collections[0] ?? '' });
		collInput.setAttribute('list', 'yeda-collections');
		const setCollection = (collection: string | null) => run(async () => {
			await invoke('set_collection', { key, collection });
			await reload();
		});
		const collection = h('div', {}, collInput, button('Move', () => setCollection(collInput.value.trim() || null)),
			r.collections.length > 0 && button('Remove from collection', () => setCollection(null)),
			datalist('yeda-collections', all('collection')));

		const fields: [string, unknown][] = [
			['key', key], ['type', r.type], ['authors', r.authors.join('; ')], ['year', r.year], ['doi', r.doi],
			['pdf', r.pdf && `${r.pdf.location}:${r.pdf.path}`], ...Object.entries(r.extra ?? {}),
		];
		const table = h('table', {}, ...fields.filter(([, v]) => v !== undefined && v !== '').map(([k, v]) =>
			h('tr', {}, h('td', {}, k), h('td', {}, typeof v === 'string' ? v : JSON.stringify(v)))));

		show(back, h('h2', {}, r.title || key),
			h('div', { className: 'actions' },
				r.pdf && button('Open PDF', () => run(() => openPdf(key))),
				button(r.pdf ? 'Replace PDF' : 'Attach PDF', () => run(() => attachPdf(key))),
				button(`Annotations (${d.annotations.length})`, () => showAnnotations(d)),
				button(d.note ? 'Note' : 'Add note', () => showText(d, 'note')),
				overview.tables.length > 0 && button('Tables', () => showTables(d)),
				button('Edit file', () => showText(d, 'raw'))),
			h('p', {}, statusSelect), tags, h('p', {}, collection), table,
			h('div', { className: 'actions' },
				r.pdf && button('Extract annotations from PDF', () => run(async () => {
					say('reading the PDF…');
					const pdf = new Uint8Array(await invoke<ArrayBuffer>('ref_pdf', { key }));
					const regions = await invoke<Region[]>('pdf_image_regions', { key });
					const { words, images } = await inspect(pdf, regions);
					const n = await invoke<number>('extract_annotations', { key, words, images });
					say(`${n} annotations extracted from the PDF`);
					await showDetails(key);
				})),
				button('Delete reference', () => run(async () => {
					if (!window.confirm(`Delete ${key}, its annotations and notes?`)) return;
					await invoke('delete_ref', { key });
					await refresh();
					await showList();
				}))));
	}

	/** Asks for a password in a masked field and saves it in the Keystore; false if cancelled. */
	function askPassword(account: string, label: string, rejected: boolean): Promise<boolean> {
		return new Promise((resolve) => {
			const input = h('input', { type: 'password', autocomplete: 'off' });
			const done = async (ok: boolean) => {
				const saved = ok && !!input.value && await run(async () => {
					await invoke('set_secret', { account, secret: input.value });
					return true;
				});
				resolve(!!saved);
			};
			show(h('h2', {}, 'WebDAV password'),
				h('p', {}, rejected ? `The server rejected the password for ${label}.` : `Password for ${label}:`), input,
				h('p', { className: 'meta' }, 'It is kept in the Android Keystore, never in the library.'),
				h('div', { className: 'actions' }, button('OK', () => done(true)), button('Cancel', () => done(false))));
			input.focus();
		});
	}

	/** Runs a command that may need a WebDAV password, asking for it until it succeeds or is cancelled. */
	async function withPassword(call: () => Promise<OpenRef>): Promise<OpenRef | null> {
		for (;;) {
			const result = await call();
			if (result.status !== 'needsPassword') return result;
			if (!(await askPassword(result.account, result.label, result.rejected))) return null;
		}
	}

	async function attachPdf(key: string) {
		if (overview.locations.length === 0) {
			say('no locations in yeda.toml to attach to');
			return;
		}
		const select = h('select', {}, ...overview.locations.map(l => h('option', { value: l }, l)));
		show(h('h2', {}, 'Attach PDF'), h('p', {}, 'Location: ', select),
			h('div', { className: 'actions' },
				button('Choose PDF', () => run(async () => {
					const path = await pickPdf();
					if (!path) return;
					say('attaching…');
					const result = await withPassword(() => invoke<OpenRef>('attach_pdf', { key, path, location: select.value }));
					if (result) say('attached');
					await refresh();
					await showDetails(key);
				})),
				button('Cancel', () => run(() => showDetails(key)))));
	}

	async function openPdf(key: string) {
		say('opening…');
		const result = await withPassword(() => invoke<OpenRef>('open_ref', { key }));
		if (!result || result.status === 'needsPassword') {
			await showDetails(key);
			return;
		}
		if (result.status === 'opened') {
			root.remove();
			style.remove();
			openReader();
			return;
		}
		if (!window.confirm(`Folder for location "${result.location}" not found on this device. Pick it now?`)) return;
		const path = await pickFolder();
		if (path) {
			await invoke('set_location', { name: result.location, path });
			await openPdf(key);
		}
	}

	function showAnnotations(d: Details) {
		const items = d.annotations.map((a) => {
			const img = a.image ? h('img', { alt: a.kind }) : null;
			if (img && a.image) {
				invoke<ArrayBuffer>('annotation_image', { key: d.key, image: a.image })
					.then(buf => img.src = URL.createObjectURL(new Blob([buf], { type: 'image/png' })))
					.catch(e => img.replaceWith(h('div', { className: 'dim' }, String(e))));
			}
			const li = h('li', { className: 'annotation' }, h('div', { className: 'meta' }, `${a.kind} · p.${a.page}`),
				a.text && h('blockquote', {}, a.text), img, a.comment && h('p', {}, a.comment));
			if (a.color) li.style.borderLeftColor = a.color;
			return li;
		});
		show(button('← Back', () => run(() => showDetails(d.key))), h('h2', {}, d.ref.title || d.key),
			items.length ? h('ul', {}, ...items) : h('p', {}, 'No annotations.'));
	}

	function showText(d: Details, which: 'note' | 'raw') {
		const area = h('textarea', { value: which === 'note' ? d.note : d.raw, className: which === 'raw' ? 'mono' : '' });
		const save = () => run(async () => {
			await invoke(which === 'note' ? 'save_note' : 'save_raw', { key: d.key, text: area.value });
			say('saved');
			await refresh();
			await showDetails(d.key);
		});
		show(button('← Back', () => run(() => showDetails(d.key))),
			h('h2', {}, which === 'note' ? `Note (Markdown): ${d.key}` : `refs/${d.key}.toml`), area,
			h('div', { className: 'actions' }, button('Save', save)));
	}

	function showTables(d: Details) {
		const sections = Object.entries(d.tables).map(([name, columns]) => {
			const row = d.ref.tables?.[name] ?? {};
			const summarize = (overwrite: boolean) => run(async () => {
				say(`summarizing ${d.key} into '${name}'…`);
				const pdf = d.ref.pdf && await invoke<ArrayBuffer>('ref_pdf', { key: d.key }).catch(() => null);
				const text = pdf ? await plainText(new Uint8Array(pdf)).catch(() => null) : null;
				say(await invoke<string>('summarize', { key: d.key, table: name, overwrite, text }));
				const fresh = await invoke<Details>('details', { key: d.key });
				showTables(fresh);
			});
			return h('section', {}, h('h2', {}, name),
				h('table', {}, ...columns.map(c => h('tr', {}, h('td', {}, c), h('td', {}, row[c] ?? '')))),
				h('div', { className: 'actions' },
					button('Fill empty cells', () => summarize(false)),
					button('Regenerate all', () => summarize(true)),
					button('Export CSV', () => run(async () => say(`exported to ${await invoke<string>('export_table', { table: name })}`)))));
		});
		show(button('← Back', () => run(() => showDetails(d.key))), ...sections);
	}

	/** Adds each identifier in a pasted list, then lists the ones that failed and copies them. */
	function addReference() {
		const area = h('textarea', { rows: 8, placeholder: 'DOIs, arXiv IDs or ISBNs, one per line or separated by spaces' });
		const add = () => run(async () => {
			const inputs = await invoke<string[]>('split_ids', { text: area.value });
			const added: string[] = [], existed: string[] = [], failed: [string, string][] = [];
			for (const [i, input] of inputs.entries()) {
				say(`fetching ${i + 1}/${inputs.length}: ${input}…`);
				try {
					const r = await invoke<{ key: string; existed: boolean }>('add_ref', { input });
					(r.existed ? existed : added).push(r.key);
				} catch (e) {
					failed.push([input, String(e)]);
				}
			}
			await refresh();
			const list = failed.map(([input]) => input).join('\n');
			// Copying can fail once the tap that started the batch is too long ago; the button retries.
			const copied = failed.length > 0 && await navigator.clipboard.writeText(list).then(() => true, () => false);
			say([added.length && `added ${added.join(', ')}`, existed.length && `already in library: ${existed.join(', ')}`,
				failed.length && `${failed.length} failed${copied ? ', copied to the clipboard' : ''}`].filter(Boolean).join(' · '));
			const last = added.at(-1) ?? existed.at(-1);
			if (failed.length === 0 && last) return showDetails(last);
			show(h('h2', {}, 'Not added'),
				h('ul', {}, ...failed.map(([input, e]) => h('li', {}, h('div', {}, input), h('div', { className: 'meta' }, e)))),
				h('div', { className: 'actions' },
					button('Copy these', () => navigator.clipboard.writeText(list).then(() => say('copied to the clipboard'), e => say(String(e)))),
					last && button('Show the last added', () => run(() => showDetails(last))),
					button('Back to the list', () => run(showList))));
		});
		show(h('h2', {}, 'Add references'), area,
			h('div', { className: 'actions' }, button('Add', add), button('Cancel', () => run(showList))));
		area.focus();
	}


	async function showSettings() {
		if (!hasFileAccess()) {
			show(h('p', {}, 'yeda needs access to all files to read and write the library folder.'),
				button('Grant file access', () => requestFileAccess().then(showSettings)));
			return;
		}
		const library = await invoke<string | null>('library');
		const keys = await run(() => invoke<{ env: string; set: boolean }[]>('llm_keys')) ?? [];
		const keyRows = keys.map(({ env, set }) => {
			const input = h('input', { type: 'password', placeholder: set ? 'set (enter to replace)' : 'not set' });
			return h('p', {}, h('div', {}, env), input, button('Save', () => run(async () => {
				await invoke('set_key', { env, key: input.value });
				say(`${env} saved`);
				await showSettings();
			})));
		});
		show(button('← Back', () => run(showList)), h('h2', {}, 'Library folder'), h('p', {}, library ?? 'None chosen'),
			button('Choose library folder', () => run(async () => {
				const path = await pickFolder();
				if (!path) return;
				await invoke('set_library', { path });
				await refresh();
				await showList();
			})),
			keyRows.length > 0 && h('h2', {}, 'LLM API keys'), ...keyRows,
			keyRows.length > 0 && h('p', { className: 'meta' }, 'Keys are kept in the Android Keystore, never in a file.'),
			h('h2', {}, 'Settings file'),
			h('p', { className: 'meta' }, 'settings.toml: the library folder plus anything the desktop ~/.config/yeda/config.toml takes, such as [locations].'),
			button('Edit settings.toml', () => run(showSettingsFile)));
	}

	async function showSettingsFile() {
		const area = h('textarea', { value: await invoke<string>('settings_text'), className: 'mono' });
		show(button('← Back', () => run(showSettings)), h('h2', {}, 'settings.toml'), area,
			h('div', { className: 'actions' }, button('Save', () => run(async () => {
				await invoke('save_settings_text', { text: area.value });
				say('settings saved');
				await refresh();
				await showSettings();
			}))));
	}

	search.addEventListener('input', () => run(showList));
	run(async () => {
		await refresh();
		if (!hasFileAccess() || !(await invoke<string | null>('library'))) await showSettings();
		else await showList();
	});
	return () => !atList && (run(showList), true);
}
