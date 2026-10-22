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
		_iframe: HTMLIFrameElement;
		_iframeWindow: (Window & { PDFViewerApplication: { pdfViewer: PdfViewer } }) | null;
		_tool: { type: string };
		setTool(tool: { type: string }): void;
		navigateToNextPage(): void;
		navigateToPreviousPage(): void;
	} | null;
}

/** Where the columns are, in fractions of the page's width and height: the crop box and the gutter. */
interface Layout { x0: number; x1: number; y0: number; y1: number; gutter: number }

/** Each step down a column keeps this much of the previous screen in view; the last may run past its end. */
const OVERLAP = 0.05;
/** How near (CSS px) a touch must be to an edge of the crop box, or the gutter, to drag it. */
const GRAB = 40;
/** The layout a paper starts with, until one is set: the last one set for any paper. */
const LAST_LAYOUT = 'yeda.columns.layout';
const WHOLE_PAGE: Layout = { x0: 0, x1: 1, y0: 0, y1: 1, gutter: 0.5 };

let on = false;

export const columnMode = () => on;

function load<T>(key: string): T | null {
	try {
		const value = localStorage.getItem(key);
		return value === null ? null : JSON.parse(value);
	}
	catch {
		return null;
	}
}

function store(key: string, value: unknown) {
	try {
		if (value === null) localStorage.removeItem(key);
		else localStorage.setItem(key, JSON.stringify(value));
	}
	catch { /* remembering is a convenience */ }
}

function toolbarButton(text: string, title: string) {
	return Object.assign(document.createElement('button'), { className: 'toolbar-button', title, textContent: text, tabIndex: -1 });
}

/**
 * Column mode, for two-column PDFs: the page is cropped to the paper's layout and split at its
 * gutter, and each column fills the screen's width and is read a screen at a time, left column,
 * then right; turning the page steps through them. The layout is set by dragging the crop box and
 * the gutter over the page. Remembered per document. Must run before the reader is created, which
 * renders the toolbar buttons; returns the hook that takes the reader and returns the toggle.
 */
export function installColumns(doc: string) {
	const toggle = toolbarButton('◫', 'Column mode (C)');
	const adjust = toolbarButton('✎', 'Adjust the columns');
	window.addEventListener('customEvent', (e) => {
		const { type, append } = (e as CustomEvent).detail;
		if (type !== 'renderToolbar') return;
		append(toggle);
		append(adjust);
	});
	return (reader: ColumnReader) => attach(reader, doc, toggle, adjust);
}

function attach(reader: ColumnReader, doc: string, toggle: HTMLButtonElement, adjust: HTMLButtonElement): () => void {
	const view = reader._primaryView;
	if (!view) return () => {};
	const onKey = `yeda.columns.${doc}`;
	const layoutKey = `yeda.columns.layout.${doc}`;
	let layout = load<Layout>(layoutKey) ?? load<Layout>(LAST_LAYOUT) ?? WHOLE_PAGE;
	const columns = () => [
		{ x0: layout.x0, x1: layout.gutter, y0: layout.y0, y1: layout.y1 },
		{ x0: layout.gutter, x1: layout.x1, y0: layout.y0, y1: layout.y1 },
	];
	const viewer = () => view._iframeWindow?.PDFViewerApplication.pdfViewer;
	/** Where reading is: the page, its column, and the step down that column. */
	let at = { page: -1, column: 0, step: 0 };
	/** Where `show` last scrolled to; anywhere else, the reader was scrolled by hand. */
	let shown = { left: NaN, top: NaN };
	let scaleBefore = 'auto';
	/** The current page once column mode was turned off; still the same when turned back on, reading carries on. */
	let pageOff = -1;

	/** The page width (CSS px) that fits column `c` to the screen, and the tops of the screens down it. */
	const fit = (v: PdfViewer, div: HTMLElement, c: number) => {
		const r = columns()[c];
		const width = v.container.clientWidth / (r.x1 - r.x0);
		const screen = v.container.clientHeight / (width * div.clientHeight / div.clientWidth);
		const tops = [r.y0];
		while (tops[tops.length - 1] + screen < r.y1) tops.push(tops[tops.length - 1] + screen * (1 - OVERLAP));
		return { width, tops };
	};

	const show = (v: PdfViewer, page: number, column: number, step: number) => {
		const div = v._pages[page]?.div;
		if (!div) return;
		const r = columns()[column];
		const { width, tops } = fit(v, div, column);
		step = step < 0 ? tops.length - 1 : Math.min(step, tops.length - 1);
		at = { page, column, step };
		if (v.currentPageNumber !== page + 1) v.currentPageNumber = page + 1;
		// Re-rendering at a new zoom is slow, and flashes on e-ink; the two columns differ by a few pixels.
		if (Math.abs(width / div.clientWidth - 1) > 0.02) v.currentScale *= width / div.clientWidth;
		// pdf.js lays the page out at its new zoom, or a newly turned-to page, on the next frame.
		requestAnimationFrame(() => {
			const c = v.container;
			const box = c.getBoundingClientRect();
			const d = div.getBoundingClientRect();
			const [x, y] = [c.scrollLeft + d.left - box.left + div.clientLeft, c.scrollTop + d.top - box.top + div.clientTop];
			c.scrollTo({
				left: x + (r.x0 + r.x1) / 2 * div.clientWidth - c.clientWidth / 2,
				top: y + tops[step] * div.clientHeight,
			});
			shown = { left: c.scrollLeft, top: c.scrollTop };
		});
	};

	const turn = (direction: 1 | -1, original: () => void) => () => {
		const v = viewer();
		if (!on || !v) return original();
		const forward = direction > 0;
		let { page, column, step } = at;
		// Scrolled elsewhere by hand: start that page over. A column's last screen can show the next
		// page, which pdf.js then counts as the current one.
		if (Math.abs(v.container.scrollTop - shown.top) > 2 || Math.abs(v.container.scrollLeft - shown.left) > 2) {
			return show(v, v.currentPageNumber - 1, forward ? 0 : 1, forward ? 0 : -1);
		}
		const div = v._pages[page]?.div;
		step += direction;
		if (div && step >= 0 && step < fit(v, div, column).tops.length) return show(v, page, column, step);
		column += direction;
		if (column === 0 || column === 1) return show(v, page, column, forward ? 0 : -1);
		page += direction;
		if (page >= 0 && page < v._pages.length) show(v, page, forward ? 0 : 1, forward ? 0 : -1);
	};
	view.navigateToNextPage = turn(1, view.navigateToNextPage.bind(view));
	view.navigateToPreviousPage = turn(-1, view.navigateToPreviousPage.bind(view));

	const set = (enable: boolean) => {
		const v = viewer();
		if (!v || enable === on) return;
		on = enable;
		store(onKey, on || null);
		toggle.classList.toggle('active', on);
		// Lets the pen code take touches, so swipes turn instead of scrolling.
		view.setTool(view._tool);
		if (!on) {
			v.currentScaleValue = scaleBefore;
			requestAnimationFrame(() => { pageOff = v.currentPageNumber; });
			return;
		}
		scaleBefore = v.currentScaleValue;
		// Back from a look at the whole page, e.g. at a figure: carry on where reading stopped.
		if (at.page >= 0 && v.currentPageNumber === pageOff) show(v, at.page, at.column, at.step);
		else show(v, v.currentPageNumber - 1, 0, 0);
	};

	/** Shows the whole page under the crop box and gutter, to drag into place. */
	const edit = () => {
		const v = viewer();
		if (!v || document.getElementById('yeda-columns-editor')) return;
		set(false);
		const before = v.currentScaleValue;
		const page = v.currentPageNumber - 1;
		v.currentScaleValue = 'page-fit';
		requestAnimationFrame(() => {
			const div = v._pages[page]?.div;
			if (!div) return;
			const frame = view._iframe.getBoundingClientRect();
			const d = div.getBoundingClientRect();
			const editor = Object.assign(document.createElement('div'), { id: 'yeda-columns-editor' });
			Object.assign(editor.style, {
				position: 'fixed', zIndex: '1000', touchAction: 'none',
				left: `${frame.left + d.left + div.clientLeft}px`, top: `${frame.top + d.top + div.clientTop}px`,
				width: `${div.clientWidth}px`, height: `${div.clientHeight}px`,
			});
			const crop = document.createElement('div');
			const gutter = document.createElement('div');
			Object.assign(crop.style, { position: 'absolute', border: '3px solid #000', outline: '2px solid #fff', boxShadow: '0 0 0 9999px rgba(0, 0, 0, 0.4)' });
			Object.assign(gutter.style, { position: 'absolute', borderLeft: '3px dashed #000', outline: '2px solid #fff' });
			const done = Object.assign(document.createElement('button'), { textContent: 'Done' });
			Object.assign(done.style, {
				position: 'fixed', zIndex: '1001', left: '50%', bottom: '24px', transform: 'translateX(-50%)',
				minWidth: '120px', minHeight: '48px', fontSize: '18px', background: '#fff', color: '#000', border: '3px solid #000',
			});
			editor.append(crop, gutter);
			document.body.append(editor, done);

			const draw = () => {
				const pct = (n: number) => `${n * 100}%`;
				Object.assign(crop.style, { left: pct(layout.x0), top: pct(layout.y0), width: pct(layout.x1 - layout.x0), height: pct(layout.y1 - layout.y0) });
				Object.assign(gutter.style, { left: pct(layout.gutter), top: pct(layout.y0), height: pct(layout.y1 - layout.y0) });
			};
			draw();

			let dragging: keyof Layout | null = null;
			const point = (e: PointerEvent) => {
				const r = editor.getBoundingClientRect();
				return { x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height, w: r.width, h: r.height };
			};
			editor.addEventListener('pointerdown', (e) => {
				const p = point(e);
				const distances: [keyof Layout, number][] = [
					['gutter', Math.abs(p.x - layout.gutter) * p.w],
					['x0', Math.abs(p.x - layout.x0) * p.w], ['x1', Math.abs(p.x - layout.x1) * p.w],
					['y0', Math.abs(p.y - layout.y0) * p.h], ['y1', Math.abs(p.y - layout.y1) * p.h],
				];
				const [nearest, distance] = distances.sort((a, b) => a[1] - b[1])[0];
				if (distance > GRAB) return;
				dragging = nearest;
				editor.setPointerCapture(e.pointerId);
			});
			editor.addEventListener('pointermove', (e) => {
				if (!dragging) return;
				const p = point(e);
				const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
				const gap = 0.05;
				const value = {
					x0: clamp(p.x, 0, layout.gutter - gap), x1: clamp(p.x, layout.gutter + gap, 1),
					y0: clamp(p.y, 0, layout.y1 - gap), y1: clamp(p.y, layout.y0 + gap, 1),
					gutter: clamp(p.x, layout.x0 + gap, layout.x1 - gap),
				}[dragging];
				layout = { ...layout, [dragging]: value };
				draw();
			});
			editor.addEventListener('pointerup', () => { dragging = null; });
			done.addEventListener('click', () => {
				editor.remove();
				done.remove();
				store(layoutKey, layout);
				store(LAST_LAYOUT, layout);
				v.currentScaleValue = before;
				at = { page, column: 0, step: 0 };
				pageOff = v.currentPageNumber;
				set(true);
			});
		});
	};

	toggle.addEventListener('click', () => set(!on));
	adjust.addEventListener('click', edit);
	// A rotated phone or a resized window fits the column again, once the reader has re-applied its own zoom.
	let refit = 0;
	window.addEventListener('resize', () => {
		clearTimeout(refit);
		refit = window.setTimeout(() => {
			const v = viewer();
			if (on && v && at.page >= 0) show(v, at.page, at.column, at.step);
		}, 300);
	});

	view.initializedPromise.then(() => {
		if (load<boolean>(onKey)) set(true);
	});
	return () => set(!on);
}
