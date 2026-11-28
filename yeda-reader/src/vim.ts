import { createCursor } from './cursor';
import type { FindState, PdfView } from './cursor';
import { showHelp } from './help';
import { JumpReader, showAnnotations, showToc } from './jump';
import { showTagPrompt } from './tags';
import type { Tag } from './tags';

export interface VimReader extends JumpReader {
	_state: Omit<JumpReader['_state'], 'annotations'> & {
		annotations: (JumpReader['_state']['annotations'][number] & {
			position: { pageIndex: number };
			_hidden?: boolean;
			readOnly?: boolean;
			tags?: Tag[];
		})[];
		selectedAnnotationIDs: string[];
		sidebarOpen: boolean;
		sidebarView: string;
		primaryViewFindState: FindState;
		readOnly: boolean;
		tool: { type: string };
	};
	_primaryView: PdfView | null;
	_tools: Record<string, { type: string; color?: string; size?: number }>;
	_annotationManager: { updateAnnotations(annotations: { id: string; color?: string; tags?: Tag[] }[]): void };
	_keyboardManager: { handleViewKeyDown(event: KeyboardEvent): void };
	navigate(location: { pageIndex: number }): void;
	navigateToFirstPage(): void;
	navigateToLastPage(): void;
	navigateToNextPage(): void;
	navigateToPreviousPage(): void;
	toggleFindPopup(params: { open: boolean }): void;
	findNext(): void;
	findPrevious(): void;
	zoomIn(): void;
	zoomOut(): void;
	zoomReset(): void;
	toggleSidebar(open?: boolean): void;
	setSidebarView(view: string): void;
	setTool(params: { type: string; color?: string }): void;
	setSelectedAnnotations(ids: string[], triggeredFromView?: boolean, event?: {
		shiftKey: boolean; ctrlKey: boolean; metaKey: boolean; key?: string;
	}): void;
	deleteAnnotations(ids: string[]): void;
	copy(): void;
	focusView(): void;
}

export interface Binding {
	keys?: string[];
	label?: string;
	help: string;
	/** Returning false leaves the key for longer bindings or the reader to handle. */
	run?: (reader: VimReader, shift: boolean, count: number) => boolean | void;
}

const COLOUR_TOOLS = ['highlight', 'underline', 'note', 'text', 'image'];
const TOOLS = [...COLOUR_TOOLS, 'ink', 'eraser'];
const COLOURS = ['#ffd400', '#ff6666', '#5fb236', '#2ea8e5', '#a28ae5', '#e56eee', '#f19837', '#aaaaaa'];
const KEEP: Record<string, true> = {
	v: true, h: true, j: true, k: true, l: true, w: true, b: true, e: true, 0: true, $: true,
	n: true, N: true, y: true, o: true, Escape: true, Enter: true, J: true, K: true, ' ': true,
	'g g': true, G: true, 'C-d': true, 'C-u': true, '/': true, '+': true, '-': true, '=': true,
};
const NO_MODIFIERS = { shiftKey: false, ctrlKey: false, metaKey: false };
const LINE = 60;

function isTextInput(target: EventTarget | null): boolean {
	const node = target as HTMLElement | null;
	return !!node && (node.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(node.nodeName));
}

function scrollContainer(): HTMLElement | undefined {
	return (window.PDFViewerApplication as { pdfViewer?: { container: HTMLElement } } | undefined)?.pdfViewer?.container;
}

function selectAnnotation(reader: VimReader, direction: 1 | -1) {
	const page = reader._primaryView?._iframeWindow?.PDFViewerApplication.pdfViewer.currentPageNumber;
	if (page === undefined) return;
	const annotations = reader._state.annotations.filter(a => !a._hidden)
		.sort((a, b) => a.sortIndex.localeCompare(b.sortIndex));
	const current = annotations.findIndex(a => a.id === reader._state.selectedAnnotationIDs[0]);
	const next = current >= 0
		? annotations[current + direction]
		: direction === 1
			? annotations.find(a => a.position.pageIndex >= page - 1)
			: annotations.reverse().find(a => a.position.pageIndex <= page - 1);
	if (next) reader.setSelectedAnnotations([next.id], false, NO_MODIFIERS);
}

function setColour(reader: VimReader, n: number) {
	const color = COLOURS[n - 1];
	if (!color) return;
	const ids = reader._state.selectedAnnotationIDs;
	if (ids.length) {
		if (reader._state.readOnly) return;
		const selected = new Set(ids);
		const updates = reader._state.annotations.filter(a => selected.has(a.id) && !a.readOnly)
			.map(a => ({ id: a.id, color }));
		if (updates.length) reader._annotationManager.updateAnnotations(updates);
	} else {
		for (const type of COLOUR_TOOLS) reader._tools[type].color = color;
		const { type } = reader._state.tool;
		if (COLOUR_TOOLS.includes(type)) reader.setTool({ type, color });
	}
}

/**
 * Routes keydown events from the main window and the PDF iframe through the vim keymap.
 * Must run before the reader is created so its capture listener fires ahead of the reader's own.
 */
export function installVim(getReader: () => VimReader | undefined, toggleColumns: () => void, close: () => void) {
	let pending: string[] = [];
	let count = '';

	const editComment = (reader: VimReader) => {
		const [id] = reader._state.selectedAnnotationIDs;
		if (!id) return;
		reader.setSidebarView('annotations');
		reader.toggleSidebar(true);
		// The editor only exists once the sidebar has rendered the selected annotation.
		requestAnimationFrame(() => {
			const editor = document.querySelector<HTMLElement>(
				`[data-sidebar-annotation-id="${id}"] .comment [contenteditable="true"]`,
			);
			if (!editor) return;
			editor.focus();
			const range = document.createRange();
			range.selectNodeContents(editor);
			range.collapse(false);
			window.getSelection()?.removeAllRanges();
			window.getSelection()?.addRange(range);
		});
	};
	const cursor = createCursor(getReader, editComment);

	const applyTool = (reader: VimReader, type: string) => {
		if (reader._state.readOnly) return;
		if (type === 'image' && (cursor.mode() === 'off' || cursor.mode() === 'caret')) {
			cursor.startArea();
			return;
		}
		const apply = () => {
			const view = reader._primaryView;
			const ranges = view?._selectionRanges;
			if ((type === 'highlight' || type === 'underline') && view && ranges?.length && !ranges[0].collapsed) {
				const added: string[] = [];
				const color = reader._tools[type].color;
				const add = (selection: PdfView['_selectionRanges']) => {
					const annotation = view._getAnnotationFromSelectionRanges(selection, type, color);
					if (!annotation) return;
					// The range already has its original sort index if its page was evicted.
					if (view._pdfPages[annotation.position.pageIndex]?.chars.length) {
						annotation.sortIndex = view.getAnnotationMeta(annotation.position).sortIndex;
					}
					const result = view._onAddAnnotation(annotation, false);
					if (result) added.push(result.id);
				};
				if (cursor.mode() === 'visual' && ranges.length > 1) {
					// One annotation can continue only onto the immediately following page.
					for (let i = 0; i < ranges.length;) {
						const end = i + (ranges[i + 1]?.position.pageIndex === ranges[i].position.pageIndex + 1 ? 2 : 1);
						add(ranges.slice(i, end));
						i = end;
					}
				} else add(ranges);
				if (added.length) {
					view.clearSelection();
					cursor.exit();
					// A keyboard selection keeps the empty comment from stealing focus.
					reader.setSelectedAnnotations(added, true, { ...NO_MODIFIERS, key: 't' });
					return;
				}
			}
			if ((type === 'note' || type === 'text') && cursor.mode() === 'caret') cursor.place(type);
			else reader.setTool({ type });
		};
		if (cursor.active()) cursor.afterMotion(apply);
		else apply();
	};

	const move = (key: Parameters<typeof cursor.move>[0], scroll?: number): NonNullable<Binding['run']> =>
		(_reader, _shift, n) => {
			if (cursor.active()) cursor.move(key, n);
			else if (scroll !== undefined) scrollContainer()?.scrollBy({ top: scroll });
			else return false;
		};

	const scrollHalf = (direction: number) => {
		const container = scrollContainer();
		container?.scrollBy({ top: direction * 0.5 * container.clientHeight });
	};

	const table: Binding[] = [
		{ keys: ['v'], help: 'Caret mode; again: select from the caret (visual); again: back to the caret', run: () => cursor.toggleVisual() },
		{ keys: ['h'], help: 'Caret: previous character', run: move('h') },
		{ keys: ['l'], help: 'Caret: next character', run: move('l') },
		{ keys: ['j'], help: 'Scroll down; caret: next line', run: move('j', LINE) },
		{ keys: ['k'], help: 'Scroll up; caret: previous line', run: move('k', -LINE) },
		{ keys: ['w'], help: 'Caret: next word', run: move('w') },
		{ keys: ['b'], help: 'Caret: word start', run: move('b') },
		{ keys: ['e'], help: 'Caret: word end', run: move('e') },
		{ keys: ['0'], help: 'Caret: line start', run: move('0') },
		{ keys: ['$'], help: 'Caret: line end', run: move('$') },
		{ keys: ['C-d'], help: 'Scroll half a page down', run: () => scrollHalf(1) },
		{ keys: ['C-u'], help: 'Scroll half a page up', run: () => scrollHalf(-1) },
		{ keys: ['J'], help: 'Next page', run: r => r.navigateToNextPage() },
		{ keys: ['K'], help: 'Previous page', run: r => r.navigateToPreviousPage() },
		{
			keys: [' '],
			label: 'Space / Shift-Space',
			help: 'Next / previous page',
			run: (r, shift) => (shift ? r.navigateToPreviousPage() : r.navigateToNextPage()),
		},
		{ keys: ['g', 'g'], help: 'First page', run: r => r.navigateToFirstPage() },
		{ keys: ['G'], help: 'Last page', run: r => r.navigateToLastPage() },
		{ label: 'NG', help: 'Go to page N' },
		{ keys: ['/'], help: 'Search', run: r => r.toggleFindPopup({ open: true }) },
		{ keys: ['n'], help: 'Next search result; caret: move there', run: (r, _shift, n) => {
			if (cursor.active()) cursor.searchMove(1, n);
			else r.findNext();
		} },
		{ keys: ['N'], help: 'Previous search result; caret: move there', run: (r, _shift, n) => {
			if (cursor.active()) cursor.searchMove(-1, n);
			else r.findPrevious();
		} },
		{ keys: ['+'], help: 'Zoom in', run: r => r.zoomIn() },
		{ keys: ['-'], help: 'Zoom out', run: r => r.zoomOut() },
		{ keys: ['='], help: 'Reset zoom', run: r => r.zoomReset() },
		{ keys: ['o'], help: 'Visual: other end of the selection; area: other corner', run: () => {
			if (cursor.mode() !== 'visual') return false;
			cursor.swap();
		} },
		{ keys: ['o', 'g'], help: 'Jump to a section', run: r => showToc(r) },
		{ keys: ['a', 'g'], help: 'Jump to an annotation', run: r => showAnnotations(r) },
		{ label: 'Nt', help: 'Tool N (highlight, underline, note, text, image, ink, eraser); with a selection, highlight or underline it; in caret mode, a note, text box or image area at the caret' },
		{ label: 'Nc', help: 'Colour N (yellow, red, green, blue, purple, magenta, orange, gray) for the selected annotations, else for the tools' },
		{ keys: ['Enter'], help: 'Area: save the image annotation', run: () => cursor.areaKey('Enter', 1) },
		{
			keys: ['Escape'],
			help: 'Leave area, visual or caret mode, else back to selection',
			// The reader also handles Escape, e.g. to clear the selection.
			run: (r) => {
				if (cursor.escape()) return true;
				r.setTool({ type: 'pointer' });
				return false;
			},
		},
		// The generated key table treats a literal closing bracket as the end of the key array.
		{ keys: ['\u005d', 'a'], help: 'Select the next annotation', run: r => selectAnnotation(r, 1) },
		{ keys: ['[', 'a'], help: 'Select the previous annotation', run: r => selectAnnotation(r, -1) },
		{ keys: ['d', 'd'], help: 'Delete the selected annotation', run: r => r.deleteAnnotations(r._state.selectedAnnotationIDs) },
		{ keys: ['c'], help: 'Edit the selected annotation\'s comment', run: r => editComment(r) },
		{ keys: ['T'], help: 'Edit the selected annotation\'s tags', run: r => {
			const ids = r._state.selectedAnnotationIDs;
			if (ids.length === 1 && !r._state.readOnly) showTagPrompt(r, ids[0]);
		} },
		{ keys: ['y'], help: 'Copy the selected text', run: r => {
			if (!cursor.active()) r.copy();
			else cursor.afterMotion(() => {
				if (cursor.mode() !== 'visual') return;
				const text = cursor.text();
				cursor.escape();
				if (text) void navigator.clipboard.writeText(text).catch(console.error);
			});
		} },
		{ keys: ['C'], help: 'Column mode', run: () => toggleColumns() },
		{ keys: ['?'], help: 'Show this help', run: r => showHelp(r, table) },
		{ keys: ['q'], help: 'Close the reader', run: () => close() },
	];

	/** Returns true when the key was consumed. */
	const handle = (event: KeyboardEvent): boolean => {
		const reader = getReader();
		if (!reader || event.altKey || event.metaKey) return false;
		if (isTextInput(event.target)) {
			// Leave the search box after running the search so `n`/`N` reach the keymap.
			if (event.key === 'Enter' && !event.shiftKey && (event.target as Element).closest('.find-popup')) {
				setTimeout(() => reader.focusView());
			}
			return false;
		}
		// A lone modifier must not reset a pending count or sequence, e.g. Shift before the `G` in `3G`.
		if (['Shift', 'Control', 'Alt', 'Meta'].includes(event.key)) return false;
		const key = event.ctrlKey ? `C-${event.key}` : event.key;

		if (/^[0-9]$/.test(key) && !pending.length && (count || key !== '0')) {
			count += key;
			return true;
		}
		const n = Number(count);
		count = '';
		const seq = [...pending, key];
		pending = [];
		if (cursor.mode() === 'area' && cursor.areaKey(key, n || 1)) return true;
		if (seq.length === 1) {
			if (key === 'G' && n) {
				reader.navigate({ pageIndex: n - 1 });
				return true;
			}
			if (key === 't') {
				const type = TOOLS[n - 1];
				if (type) applyTool(reader, type);
				return true;
			}
			if (key === 'c' && n) {
				setColour(reader, n);
				return true;
			}
		}

		const id = seq.join(' ');
		const binding = bindings.get(id);
		if (binding) {
			if (cursor.active() && !Object.hasOwn(KEEP, id)) cursor.exit();
			if (binding.run!(reader, event.shiftKey, n || 1) !== false) return true;
		}
		if (prefixes.has(id)) {
			pending = seq;
			return true;
		}
		return false;
	};

	const bindings = new Map<string, Binding>();
	const prefixes = new Set<string>();
	for (const binding of table) {
		if (!binding.run) continue;
		const keys = binding.keys!;
		bindings.set(keys.join(' '), binding);
		for (let i = 1; i < keys.length; i++) prefixes.add(keys.slice(0, i).join(' '));
	}

	const consume = (event: KeyboardEvent) => {
		if (!handle(event)) return false;
		event.preventDefault();
		event.stopImmediatePropagation();
		return true;
	};
	window.addEventListener('keydown', consume, true);

	return (reader: VimReader) => {
		const view = reader._primaryView;
		if (view) {
			// The PDF view can consume keys such as Escape before forwarding them to the reader.
			const original = view._handleKeyDown.bind(view);
			view._handleKeyDown = (event) => {
				if (!consume(event)) original(event);
			};
		}

		const keyboard = reader._keyboardManager;
		const original = keyboard.handleViewKeyDown.bind(keyboard);
		keyboard.handleViewKeyDown = (event) => {
			const primaryDocument = view?._iframeWindow?.document;
			const fromPrimary = primaryDocument && (event.target as Node | null)?.ownerDocument === primaryDocument;
			if (fromPrimary || !consume(event)) original(event);
		};
	};
}
