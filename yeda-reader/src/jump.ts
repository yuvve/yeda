interface OutlineItem {
	title: string;
	location?: { position?: { pageIndex: number } };
	items?: OutlineItem[];
}

interface JumpAnnotation {
	id: string;
	type: string;
	sortIndex: string;
	pageLabel?: string;
	color?: string;
	text?: string;
	comment?: string;
}

export interface JumpReader {
	_state: { outline: OutlineItem[] | null; annotations: JumpAnnotation[] };
	navigate(location: object, options?: object): void;
	focusView(): void;
}

export interface Entry {
	label: string;
	search?: string;
	detail: string;
	color?: string;
	indent: number;
	go: () => void;
}

/** Scores `query` as a subsequence of `text`; higher is better, -1 means no match. */
export function fuzzyScore(query: string, text: string): number {
	const q = query.toLowerCase();
	const t = text.toLowerCase();
	let score = 0;
	let at = 0;
	let streak = 0;
	for (const ch of q) {
		const i = t.indexOf(ch, at);
		if (i < 0) return -1;
		streak = i === at ? streak + 1 : 0;
		score += 1 + streak * 2 + (i === 0 || /\W/.test(t[i - 1]) ? 3 : 0) - Math.min(i - at, 5) * 0.1;
		at = i + 1;
	}
	return score - t.length * 0.01;
}

function tocEntries(reader: JumpReader): Entry[] {
	const entries: Entry[] = [];
	const walk = (items: OutlineItem[], indent: number) => {
		for (const item of items) {
			const { location } = item;
			if (location) {
				const page = location.position ? String(location.position.pageIndex + 1) : '';
				entries.push({
					label: item.title,
					detail: page,
					indent,
					go: () => reader.navigate(location, { block: 'start' }),
				});
			}
			walk(item.items ?? [], indent + 1);
		}
	};
	walk(reader._state.outline ?? [], 0);
	return entries;
}

function annotationEntries(reader: JumpReader): Entry[] {
	return [...reader._state.annotations]
		.sort((a, b) => (a.sortIndex < b.sortIndex ? -1 : a.sortIndex > b.sortIndex ? 1 : 0))
		.map(a => ({
			label: [a.text, a.comment].filter(Boolean).join(' — ') || `(${a.type})`,
			detail: `${a.type} p. ${a.pageLabel ?? ''}`,
			color: a.color,
			indent: 0,
			go: () => reader.navigate({ annotationID: a.id }),
		}));
}

let open = false;

export function showPopup(reader: JumpReader, title: string, entries: Entry[], empty: string) {
	if (open) return;
	open = true;

	const overlay = document.createElement('div');
	overlay.style.cssText = 'position:fixed;inset:0;z-index:10000;display:flex;justify-content:center;align-items:flex-start;padding-top:10vh;background:rgba(0,0,0,.3)';
	const box = document.createElement('div');
	box.style.cssText = 'width:min(640px,90vw);max-height:70vh;display:flex;flex-direction:column;background:Canvas;color:CanvasText;border:1px solid GrayText;border-radius:6px;font:14px sans-serif;box-shadow:0 8px 32px rgba(0,0,0,.4)';
	const input = document.createElement('input');
	input.placeholder = title;
	input.style.cssText = 'padding:10px;border:0;border-bottom:1px solid GrayText;background:transparent;color:inherit;font:inherit;outline:none';
	const list = document.createElement('div');
	list.style.cssText = 'overflow-y:auto';
	box.append(input, list);
	overlay.append(box);

	let shown: Entry[] = [];
	let pick = 0;

	const render = () => {
		list.replaceChildren();
		if (!shown.length) {
			list.append(Object.assign(document.createElement('div'), {
				textContent: entries.length ? 'No match' : empty,
				style: 'padding:10px;color:GrayText',
			}));
			return;
		}
		shown.forEach((entry, i) => {
			const row = document.createElement('div');
			row.style.cssText = `display:flex;gap:8px;padding:6px 10px 6px ${10 + entry.indent * 16}px;cursor:pointer;${i === pick ? 'background:Highlight;color:HighlightText' : ''}`;
			if (entry.color) {
				const swatch = document.createElement('span');
				swatch.style.cssText = `flex:none;width:10px;height:10px;margin-top:4px;border-radius:2px;background:${entry.color}`;
				row.append(swatch);
			}
			const label = Object.assign(document.createElement('span'), { textContent: entry.label });
			label.style.cssText = 'flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap';
			const detail = Object.assign(document.createElement('span'), { textContent: entry.detail });
			detail.style.cssText = 'flex:none;opacity:.7';
			row.append(label, detail);
			row.addEventListener('mousedown', (e) => {
				e.preventDefault();
				choose(entry);
			});
			list.append(row);
			if (i === pick) row.scrollIntoView({ block: 'nearest' });
		});
	};

	const filter = () => {
		const query = input.value.trim();
		shown = query
			? entries
				.map(entry => ({ entry, score: fuzzyScore(query, entry.search ?? entry.label) }))
				.filter(m => m.score >= 0)
				.sort((a, b) => b.score - a.score)
				.map(m => m.entry)
			: entries;
		pick = 0;
		render();
	};

	const close = () => {
		overlay.remove();
		open = false;
		reader.focusView();
	};
	const choose = (entry: Entry) => {
		close();
		entry.go();
	};

	input.addEventListener('input', filter);
	input.addEventListener('keydown', (event) => {
		event.stopPropagation();
		const down = event.key === 'ArrowDown' || (event.ctrlKey && event.key === 'n');
		const up = event.key === 'ArrowUp' || (event.ctrlKey && event.key === 'p');
		if (event.key === 'Escape') close();
		else if (event.key === 'Enter') {
			if (shown[pick]) choose(shown[pick]);
		}
		else if (down || up) {
			pick = Math.max(0, Math.min(shown.length - 1, pick + (down ? 1 : -1)));
			render();
		}
		else return;
		event.preventDefault();
	});
	input.addEventListener('blur', () => {
		if (open) close();
	});

	document.body.append(overlay);
	filter();
	input.focus();
}

export function showToc(reader: JumpReader) {
	const empty = reader._state.outline === null ? 'Loading the table of contents…' : 'This PDF has no table of contents';
	showPopup(reader, 'Jump to section', tocEntries(reader), empty);
}

export function showAnnotations(reader: JumpReader) {
	showPopup(reader, 'Jump to annotation', annotationEntries(reader), 'No annotations');
}
