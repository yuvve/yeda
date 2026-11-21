import { showHelp } from './help';
import { JumpReader, showAnnotations, showToc } from './jump';

export interface VimReader extends JumpReader {
	_state: JumpReader['_state'] & { selectedAnnotationIDs: string[]; sidebarOpen: boolean; sidebarView: string };
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
	setTool(params: { type: string }): void;
	deleteAnnotations(ids: string[]): void;
	copy(): void;
	focusView(): void;
}

export interface Binding {
	keys?: string[];
	label?: string;
	help: string;
	/** Returning false leaves the key for the reader to handle too. */
	run?: (reader: VimReader, shift: boolean) => boolean | void;
}

const TOOLS = ['highlight', 'underline', 'note', 'text', 'image', 'ink', 'eraser'];
const LINE = 60;

function isTextInput(target: EventTarget | null): boolean {
	const node = target as HTMLElement | null;
	return !!node && (node.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(node.nodeName));
}

function scrollContainer(): HTMLElement | undefined {
	return (window.PDFViewerApplication as { pdfViewer?: { container: HTMLElement } } | undefined)?.pdfViewer?.container;
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

	const scrollHalf = (direction: number) => {
		const container = scrollContainer();
		container?.scrollBy({ top: direction * 0.5 * container.clientHeight });
	};

	const table: Binding[] = [
		{ keys: ['j'], help: 'Scroll down', run: () => scrollContainer()?.scrollBy({ top: LINE }) },
		{ keys: ['k'], help: 'Scroll up', run: () => scrollContainer()?.scrollBy({ top: -LINE }) },
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
		{ keys: ['n'], help: 'Next search result', run: r => r.findNext() },
		{ keys: ['N'], help: 'Previous search result', run: r => r.findPrevious() },
		{ keys: ['+'], help: 'Zoom in', run: r => r.zoomIn() },
		{ keys: ['-'], help: 'Zoom out', run: r => r.zoomOut() },
		{ keys: ['='], help: 'Reset zoom', run: r => r.zoomReset() },
		{ keys: ['o', 'g'], help: 'Jump to a section', run: r => showToc(r) },
		{ keys: ['a', 'g'], help: 'Jump to an annotation', run: r => showAnnotations(r) },
		{ label: 'Nt', help: 'Select tool N: highlight, underline, note, text, image, ink, eraser' },
		{
			keys: ['Escape'],
			help: 'Back to selection',
			// The reader also handles Escape, e.g. to clear the selection.
			run: (r) => {
				r.setTool({ type: 'pointer' });
				return false;
			},
		},
		{ keys: ['d', 'd'], help: 'Delete the selected annotation', run: r => r.deleteAnnotations(r._state.selectedAnnotationIDs) },
		{ keys: ['c'], help: 'Edit the selected annotation\'s comment', run: r => editComment(r) },
		{ keys: ['y'], help: 'Copy the selected text', run: r => r.copy() },
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

		if (/^[0-9]$/.test(key) && !pending && (count || key !== '0')) {
			count += key;
			return true;
		}
		const n = Number(count);
		count = '';
		if (key === 'G' && n) {
			reader.navigate({ pageIndex: n - 1 });
			return true;
		}
		if (key === 't') {
			if (TOOLS[n - 1]) reader.setTool({ type: TOOLS[n - 1] });
			return true;
		}

		const seq = [...pending, key];
		pending = [];
		const id = seq.join(' ');

		const binding = bindings.get(id);
		if (binding) return binding.run!(reader, event.shiftKey) !== false;
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

	window.addEventListener('keydown', (event) => {
		if (handle(event)) {
			event.preventDefault();
			event.stopImmediatePropagation();
		}
	}, true);

	// Keys pressed inside the PDF iframe reach the reader through this hook instead.
	return (reader: VimReader) => {
		const keyboard = reader._keyboardManager;
		const original = keyboard.handleViewKeyDown.bind(keyboard);
		keyboard.handleViewKeyDown = (event) => {
			if (handle(event)) {
				event.preventDefault();
				event.stopPropagation();
				return;
			}
			original(event);
		};
	};
}
