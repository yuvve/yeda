import { invoke } from '@tauri-apps/api/core';
import { hasFileAccess, pickFolder, pickPdf, requestFileAccess } from './bridge';
import { einkMode, setEinkMode } from '../../yeda-reader/src/pen';
import { inspect, Region } from './pdftext';

type Filter =
	| { kind: 'all' }
	| { kind: 'search'; value: [string, string] }
	| { kind: 'collection'; value: string }
	| { kind: 'tag'; value: string };

interface Overview {
	sidebar: { filter: Filter; color: string | null; sort: Sort | null }[];
	statuses: string[];
	filter_fields: [string, string][];
	sort_fields: [string, string][];
	locations: string[];
	git: string | null;
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
}

interface Details {
	key: string;
	ref: Ref;
	raw: string;
	note: string;
	annotations: Annotation[];
}

type OpenRef =
	| { status: 'opened' }
	| { status: 'missingLocation'; location: string }
	| { status: 'needsPassword'; account: string; label: string; rejected: boolean }
	| { status: 'synced'; message: string };

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

const VIEW_KEY = 'yeda-browser-view';

/** Shows the library browser; `openReader` switches to the reader once a PDF is open. */
/** Returns the Back handler: true if it went back to the list, false if already there. */
export function showBrowser(openReader: () => void): () => boolean {
	const style = h('style', { textContent: STYLE });
	const status = h('div', { className: 'status' });
	const main = h('main');
	const search = h('input', { type: 'search', placeholder: 'Search (author:smith year:2020 …)' });
	const filterButton = h('button');
	const sortButton = h('button', { onclick: () => showSortMenu() });
	const syncButton = button('⟳', () => run(sync));
	const root = h('div', { className: 'yeda' },
		h('div', { className: 'bar' }, filterButton, search, button('Filter', () => run(showFilterForm)),
			sortButton, syncButton, button('⚙', () => showSettings())),
		main, status);
	document.body.append(style, root);

	let overview: Overview = { sidebar: [], statuses: [], filter_fields: [], sort_fields: [], locations: [], git: null };
	// Opening the reader reloads the page, so the list's view has to outlive it.
	const saved = JSON.parse(localStorage.getItem(VIEW_KEY) ?? '{}');
	let filter: Filter = saved.filter ?? { kind: 'all' };
	let sort: Sort = saved.sort ?? { field: 'key', descending: false };
	search.value = saved.query ?? '';

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
		syncButton.hidden = !overview.git;
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
		localStorage.setItem(VIEW_KEY, JSON.stringify({ filter, sort, query: search.value }));
		const list = h('ul', {}, ...rows.map(r => h('li', { onclick: () => run(() => showDetails(r.key)) },
			h('div', {}, `${GLYPHS[r.status ?? ''] ?? '·'} `, r.title || r.key),
			h('div', { className: 'meta' }, [r.authors[0]?.split(',')[0], r.year].filter(Boolean).join(' · '),
				r.has_pdf ? '' : ' · no PDF'),
			r.tags.length > 0 && h('div', {}, ...r.tags.map(chip)))));
		const saveSearch = search.value.trim() && button('Save this search', () => run(async () => {
			const name = window.prompt('Name for this search');
			if (!name) return;
			await invoke('save_search', { name, query: search.value, sort, old: null });
			filter = { kind: 'search', value: [name, search.value] };
			search.value = '';
			await refresh();
			await showList();
		}));
		show(h('div', { className: 'meta' }, `${rows.length} references`), saveSearch, list);
		atList = true;
	}

	/** Android's `window.prompt` ignores its default value, so the current colour needs a form. */
	function showTagColor(tag: string, color: string | null) {
		const text = h('input', { value: color ?? '', placeholder: '#rrggbb or a name like red' });
		const picker = h('input', { type: 'color', style: 'padding: 0; width: 3em; height: 2.5em; vertical-align: middle', value: /^#[0-9a-f]{6}$/i.test(color ?? '') ? color! : '#000000' });
		picker.oninput = () => text.value = picker.value;
		const save = (value: string) => run(async () => {
			await invoke('set_tag_color', { tag, color: value });
			await refresh();
			showSidebar();
		});
		show(h('h2', {}, `Colour for #${tag}`), h('p', {}, picker, ' ', text),
			h('div', { className: 'actions' }, button('Save', () => save(text.value.trim())), button('Remove colour', () => save('')),
				button('Cancel', () => showSidebar())));
		text.focus();
	}

	function showSearchEdit(old: string, query: string, order: Sort | null) {
		const name = h('input', { value: old });
		const text = h('input', { value: query, autocapitalize: 'off' });
		const field = h('select', {}, h('option', { value: '' }, 'unsorted'),
			...overview.sort_fields.map(([f, label]) => h('option', { value: f, selected: f === order?.field }, label)));
		const descending = h('input', { type: 'checkbox', checked: order?.descending ?? false });
		const save = () => run(async () => {
			const sortBy = field.value ? { field: field.value, descending: descending.checked } : null;
			await invoke('save_search', { name: name.value.trim(), query: text.value, sort: sortBy, old });
			filter = { kind: 'search', value: [name.value.trim(), text.value] };
			if (sortBy) sort = sortBy;
			await refresh();
			await showList();
		});
		show(h('h2', {}, 'Saved search'),
			h('table', {}, h('tr', {}, h('td', {}, 'Name'), h('td', {}, name)), h('tr', {}, h('td', {}, 'Query'), h('td', {}, text)),
				h('tr', {}, h('td', {}, 'Sort'), h('td', {}, field, ' ', h('label', { className: 'check' }, descending, ' descending')))),
			h('div', { className: 'actions' }, button('Save', save), button('Cancel', () => showSidebar())));
	}

	function showSidebar() {
		const entries = overview.sidebar.map(({ filter: f, color, sort: order }) => {
			const label = h('span', { style: color ? `color: ${color}` : '' }, filterLabel(f));
			const pick = () => run(async () => {
				filter = f;
				if (order) sort = order;
				await refresh();
				await showList();
			});
			const extra = f.kind === 'tag'
				? button('colour', () => showTagColor(f.value, color))
				: f.kind === 'search' && h('span', {}, button('edit', () => showSearchEdit(f.value[0], f.value[1], order)), ' ',
					button('delete', () => run(async () => {
						if (!window.confirm(`Delete saved search "${f.value[0]}"?`)) return;
						await invoke('delete_search', { name: f.value[0] });
						filter = { kind: 'all' };
						await refresh();
						showSidebar();
					})));
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
			const git = account.startsWith('git:');
			const what = git ? 'fine-grained access token' : 'password';
			show(h('h2', {}, what[0].toUpperCase() + what.slice(1)),
				h('p', {}, rejected ? `The server rejected the ${what} for ${label}.` : `The ${what} for ${label}:`), input,
				git && h('p', { className: 'meta' },
					'On GitHub: Settings → Developer settings → Fine-grained tokens. Repository access: only select repositories, ' +
					'with the library\'s repository chosen. Permissions: Contents, read and write.'),
				h('p', { className: 'meta' }, 'It is kept in the Android Keystore, never in the library.'),
				h('div', { className: 'actions' }, button('OK', () => done(true)), button('Cancel', () => done(false))));
			input.focus();
		});
	}

	async function sync() {
		say('syncing…');
		const result = await withPassword(() => invoke<OpenRef>('git_sync'));
		say(result?.status === 'synced' ? result.message : 'sync cancelled');
		await refresh();
		await showList();
	}

	async function cloneLibrary(url: string) {
		const path = await pickFolder();
		if (!path) return;
		say('cloning…');
		const result = await withPassword(() => invoke<OpenRef>('git_clone', { url, path }));
		if (result?.status !== 'synced') return say('clone cancelled');
		say(`cloned into ${path}`);
		await refresh();
		await showList();
	}

	/** Runs a command that may need a password, asking for it until it succeeds or is cancelled. */
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
		if (result.status !== 'missingLocation') return;
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

	async function showSettings() {
		if (!hasFileAccess()) {
			show(h('p', {}, 'yeda needs access to all files to read and write the library folder.'),
				button('Grant file access', () => requestFileAccess().then(showSettings)));
			return;
		}
		const library = await invoke<string | null>('library');
		const cloneUrl = h('input', { type: 'url', placeholder: 'https://github.com/user/library.git' });
		show(button('← Back', () => run(showList)), h('h2', {}, 'Library folder'), h('p', {}, library ?? 'None chosen'),
			button('Choose library folder', () => run(async () => {
				const path = await pickFolder();
				if (!path) return;
				await invoke('set_library', { path });
				await refresh();
				await showList();
			})),
			h('h2', {}, 'Git'),
			overview.git
				? h('div', {}, h('p', {}, overview.git), button('Sync now', () => run(sync)))
				: h('div', {}, h('p', { className: 'meta' }, 'Clone a library repository over HTTPS into an empty folder; it becomes the library.'),
					cloneUrl, button('Clone into a folder', () => cloneUrl.value.trim() && run(() => cloneLibrary(cloneUrl.value.trim())))),
			h('h2', {}, 'WebDAV location'), webdavForm(),
			h('h2', {}, 'Settings file'),
			h('p', { className: 'meta' }, 'settings.toml: the library folder, and [locations] for folders elsewhere on this device.'),
			button('Edit settings.toml', () => run(showSettingsFile)),
			h('h2', {}, 'Display'), einkToggle(),
			h('h2', {}, 'Help'), button('Search syntax and screens', showHelp));
	}

	function webdavForm() {
		const field = (placeholder: string, type = 'text') => h('input', { type, placeholder, autocapitalize: 'off' });
		const name = field('name, e.g. dav'), url = field('https://dav.example.com/pdfs/', 'url'), user = field('user'), password = field('password', 'password');
		return h('div', {}, h('p', { className: 'meta' }, 'Adds or replaces [locations.<name>] in yeda.toml after a test login; the password goes to the Keystore.'),
			name, url, user, password,
			button('Test and save', () => run(async () => {
				await invoke('setup_webdav', { name: name.value, url: url.value, user: user.value, password: password.value });
				say(`saved location ${name.value.trim()}`);
				await refresh();
				await showSettings();
			})));
	}

	function einkToggle() {
		const box = h('input', { type: 'checkbox', checked: einkMode() });
		box.addEventListener('change', () => setEinkMode(box.checked));
		return h('label', { className: 'check' }, box,
			' E-ink screen: in the reader, pages turn instead of scrolling, nothing animates, and ink is black.');
	}

	function showHelp() {
		const section = (title: string, ...lines: string[]) => [h('h2', {}, title), h('ul', {}, ...lines.map(l => h('li', {}, l)))];
		show(button('← Back', () => run(showSettings)),
			...section('Search',
				'Words separated by spaces must all match (AND), in any field, fuzzily.',
				'a|b matches either (OR); !word excludes it (NOT).',
				'field:value searches one field: author, title, year, tag, coll, status, type, doi, key, or any field of the reference.',
				'Quote values with spaces: status:"To Read". An empty value ("") finds references without that field.',
				'Values are fuzzy; ^ and $ anchor them: status:"^Read$".'),
			...section('Browsing',
				'The first button picks a collection, tag or saved search; "Save this search" under a search keeps it with the current sort, and edit there changes its name, query or sort.',
				'Filter opens a form with one box per field and suggestions from the library.',
				'The sort button picks the order; picking the current field again reverses it.'),
			...section('A reference',
				'Open PDF reads and annotates it; annotations are saved to the library as you go.',
				'Status, tags and collection are set on the reference page; tag colours from the sidebar.',
				'Extract annotations from PDF imports the annotations already embedded in the file.'),
			...section('Sync', '⟳ commits, pulls and pushes the library with git, when the library folder is a git repository.'));
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
