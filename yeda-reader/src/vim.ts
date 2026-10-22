export interface VimReader {
	_state: { selectedAnnotationIDs: string[]; sidebarOpen: boolean; sidebarView: string };
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
export function installVim(getReader: () => VimReader | undefined, toggleColumns: () => void) {
	let pending = '';
	let count = '';

	const toggleSidebarView = (reader: VimReader, view: string) => {
		const { sidebarOpen, sidebarView } = reader._state;
		if (sidebarOpen && sidebarView === view) {
			reader.toggleSidebar(false);
		}
		else {
			reader.setSidebarView(view);
			reader.toggleSidebar(true);
		}
	};

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

		const seq = pending + key;
		pending = '';

		switch (seq) {
			case 'j': scrollContainer()?.scrollBy({ top: LINE }); return true;
			case 'k': scrollContainer()?.scrollBy({ top: -LINE }); return true;
			case 'C-d':
			case 'C-u': {
				const container = scrollContainer();
				container?.scrollBy({ top: (key === 'C-d' ? 0.5 : -0.5) * container.clientHeight });
				return true;
			}
			case 'J': reader.navigateToNextPage(); return true;
			case 'K': reader.navigateToPreviousPage(); return true;
			case ' ':
				if (event.shiftKey) reader.navigateToPreviousPage();
				else reader.navigateToNextPage();
				return true;
			case 'gg': reader.navigateToFirstPage(); return true;
			case 'G': reader.navigateToLastPage(); return true;
			case '/': reader.toggleFindPopup({ open: true }); return true;
			case 'n': reader.findNext(); return true;
			case 'N': reader.findPrevious(); return true;
			case '+': reader.zoomIn(); return true;
			case '-': reader.zoomOut(); return true;
			case '=': reader.zoomReset(); return true;
			case 'o': toggleSidebarView(reader, 'outline'); return true;
			case 'a': toggleSidebarView(reader, 'annotations'); return true;
			case 'dd': reader.deleteAnnotations(reader._state.selectedAnnotationIDs); return true;
			case 'c': editComment(reader); return true;
			case 'y': reader.copy(); return true;
			case 'C': toggleColumns(); return true;
			case 'Escape':
				reader.setTool({ type: 'pointer' });
				// The reader also handles Escape, e.g. to clear the selection.
				return false;
			case 'g':
			case 'd':
				pending = seq;
				return true;
		}
		return false;
	};

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
