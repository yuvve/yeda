interface PdfViewer {
	container: HTMLElement;
	currentPageNumber: number;
	currentScale: number;
	currentScaleValue: string;
	_pages: { div: HTMLElement }[];
}

export interface ColumnReader {
	_primaryView: {
		initializedPromise: Promise<unknown>;
		_iframeWindow: (Window & { PDFViewerApplication: { pdfViewer: PdfViewer } }) | null;
		navigateToNextPage(): void;
		navigateToPreviousPage(): void;
	} | null;
}

let on = false;

export const columnMode = () => on;

/**
 * Column mode, for two-column PDFs: each page is read as four quadrants, top-left, bottom-left,
 * top-right, bottom-right, each zoomed to fill the screen, and turning the page steps through them.
 * Remembered per document. Must run before the reader is created, which renders the toolbar
 * button; returns the hook that takes the reader and returns the toggle.
 */
export function installColumns(doc: string) {
	const button = Object.assign(document.createElement('button'), {
		className: 'toolbar-button',
		title: 'Column mode (C)',
		textContent: '◫',
		tabIndex: -1,
	});
	window.addEventListener('customEvent', (e) => {
		const { type, append } = (e as CustomEvent).detail;
		if (type === 'renderToolbar') append(button);
	});
	return (reader: ColumnReader) => attach(reader, doc, button);
}

function attach(reader: ColumnReader, doc: string, button: HTMLButtonElement): () => void {
	const view = reader._primaryView;
	if (!view) return () => {};
	const key = `yeda.columns.${doc}`;
	const viewer = () => view._iframeWindow?.PDFViewerApplication.pdfViewer;
	let at = { page: -1, quadrant: 0 };
	let scaleBefore = 'auto';

	const show = (v: PdfViewer, page: number, quadrant: number) => {
		at = { page, quadrant };
		const div = v._pages[page]?.div;
		if (!div) return;
		const c = v.container;
		const r = div.getBoundingClientRect();
		const box = c.getBoundingClientRect();
		const left = c.scrollLeft + r.left - box.left + (quadrant >= 2 ? r.width / 2 : 0);
		const top = c.scrollTop + r.top - box.top + (quadrant % 2 ? r.height / 2 : 0);
		c.scrollTo({ left: left + r.width / 4 - c.clientWidth / 2, top });
	};

	const turn = (direction: 1 | -1, original: () => void) => () => {
		const v = viewer();
		if (!on || !v) return original();
		const page = v.currentPageNumber - 1;
		// Scrolled elsewhere by hand: start that page over.
		if (page !== at.page) at = { page, quadrant: direction > 0 ? -1 : 4 };
		const quadrant = at.quadrant + direction;
		if (quadrant >= 0 && quadrant <= 3) return show(v, page, quadrant);
		const next = page + direction;
		if (next < 0 || next >= v._pages.length) return;
		v.currentPageNumber = next + 1;
		// pdf.js lays the new page out on the next frame.
		requestAnimationFrame(() => show(v, next, direction > 0 ? 0 : 3));
	};
	view.navigateToNextPage = turn(1, view.navigateToNextPage.bind(view));
	view.navigateToPreviousPage = turn(-1, view.navigateToPreviousPage.bind(view));

	const set = (enable: boolean) => {
		const v = viewer();
		if (!v) return;
		on = enable;
		try {
			if (on) localStorage.setItem(key, '1');
			else localStorage.removeItem(key);
		}
		catch { /* remembering is a convenience */ }
		button.classList.toggle('active', on);
		const page = v.currentPageNumber - 1;
		if (!on) {
			v.currentScaleValue = scaleBefore;
			return;
		}
		scaleBefore = v.currentScaleValue;
		const div = v._pages[page]?.div;
		if (!div) return;
		const c = v.container;
		v.currentScale *= Math.min(c.clientWidth / (div.offsetWidth / 2), c.clientHeight / (div.offsetHeight / 2));
		// Back from a look at the whole page, e.g. at a full-width figure: carry on where reading stopped.
		requestAnimationFrame(() => show(v, page, at.page === page ? at.quadrant : 0));
	};

	button.addEventListener('click', () => set(!on));

	view.initializedPromise.then(() => {
		let saved = false;
		try { saved = localStorage.getItem(key) === '1'; }
		catch { /* remembering is a convenience */ }
		if (saved) set(true);
	});
	return () => set(!on);
}
