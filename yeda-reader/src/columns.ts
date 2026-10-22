import type { PdfPage } from './annotations';

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
		_iframeWindow: (Window & { PDFViewerApplication: { pdfViewer: PdfViewer; pdfDocument?: { getPage(n: number): Promise<PdfPage> } } }) | null;
		_tool: { type: string };
		setTool(tool: { type: string }): void;
		navigateToNextPage(): void;
		navigateToPreviousPage(): void;
	} | null;
}

/** A part of a page read as one, in fractions of the page's width and height, top-down. */
export interface Region { x0: number; x1: number; y0: number; y1: number }

/** Lines crossing the middle of the text by more than this (page fraction) span both columns. */
const CROSS = 0.02;
/** Pieces of text on one line this close (page fraction) are one line; columns are further apart. */
const WORD_GAP = 0.015;
/** Full-width lines closer than this (page fraction) belong to one full-width part. */
const JOIN = 0.03;
/** Running heads, page numbers and footers sit in this much of the top and bottom (page fraction). */
const EDGE = 0.05;
/** Text in both columns for less than this (page fraction) is an equation, table or list of authors, read across. */
const MIN_COLUMNS = 0.08;
/** A lone full-width line this thin (page fraction), ending this near the top, is a running head. */
const HEAD = [0.02, 0.1];
/** Room around a region, in page fractions, so descenders and the outermost letters aren't cut. */
const PAD = 0.01;
/** Each step down a region keeps this much of the previous screen in view; the last may run past its end. */
const OVERLAP = 0.05;

let on = false;

export const columnMode = () => on;

/**
 * The reading order of a page: above, between and below the parts that span both columns, the left
 * column, then the right. Spanning parts reach up to the column text above them, so the figure
 * over a full-width caption comes with it. A one-column page is one region.
 */
export function regions(pieces: Region[]): Region[] {
	const lines = joinLines(pieces.filter(l => l.y0 > EDGE && l.y1 < 1 - EDGE));
	if (!lines.length) return [{ x0: 0, x1: 1, y0: 0, y1: 1 }];
	const left = Math.min(...lines.map(l => l.x0));
	const right = Math.max(...lines.map(l => l.x1));
	const mid = (left + right) / 2;
	const spans = (l: Region) => l.x0 < mid - CROSS && l.x1 > mid + CROSS;

	const bands: { y0: number; y1: number }[] = [];
	for (const l of lines.filter(spans).sort((a, b) => a.y0 - b.y0)) {
		const last = bands[bands.length - 1];
		if (last && l.y0 - last.y1 < JOIN) last.y1 = Math.max(last.y1, l.y1);
		else bands.push({ y0: l.y0, y1: l.y1 });
	}
	if (bands[0] && bands[0].y1 < HEAD[1] && bands[0].y1 - bands[0].y0 < HEAD[0]) bands.shift();
	const columnLines = lines.filter(l => !spans(l) && !bands.some(b => l.y0 >= b.y0 && l.y1 <= b.y1));

	const out: Region[] = [];
	/** Extends the full-width region above, if that is the last one, to `y1`. */
	const across = (y0: number, y1: number) => {
		const last = out[out.length - 1];
		if (last && last.x0 === left && last.x1 === right) last.y1 = Math.max(last.y1, y1);
		else out.push({ x0: left, x1: right, y0, y1 });
	};
	let top = 0;
	// Where the last region's content ends; a figure over a full-width caption has no text to find it by.
	let end = EDGE;
	for (const band of [...bands, null]) {
		const between = columnLines.filter(l => l.y0 >= top && l.y1 <= (band?.y0 ?? 1));
		const ys = between.flatMap(l => [l.y0, l.y1]);
		const [y0, y1] = [out.length ? Math.min(...ys) : EDGE, Math.max(...ys)];
		if (y1 - y0 > MIN_COLUMNS && between.some(l => l.x1 <= mid + CROSS) && between.some(l => l.x0 >= mid - CROSS)) {
			// Both columns get the height of the taller, which takes in a figure without text in the other.
			out.push({ x0: left, x1: mid, y0, y1 }, { x0: mid, x1: right, y0, y1 });
			end = y1;
			if (band) across(end, band.y1);
		}
		// Text on one side only, such as a paragraph's last line on a one-column page, is read across.
		else if (band || ys.length) across(end, Math.max(band?.y1 ?? 0, ...ys));
		end = out[out.length - 1]?.y1 ?? end;
		top = band?.y1 ?? 1;
	}
	return out.map(r => ({ x0: r.x0 - PAD, x1: r.x1 + PAD, y0: r.y0 - PAD, y1: r.y1 + PAD }));
}

/** pdf.js splits a line wherever the font changes, e.g. at every superscript in a list of authors. */
function joinLines(pieces: Region[]): Region[] {
	const lines: Region[] = [];
	for (const p of [...pieces].sort((a, b) => a.y0 - b.y0 || a.x0 - b.x0)) {
		const line = lines.find(l => p.y0 < l.y1 && p.y1 > l.y0 && p.x0 - l.x1 < WORD_GAP && l.x0 - p.x1 < WORD_GAP);
		if (line) Object.assign(line, { x0: Math.min(line.x0, p.x0), x1: Math.max(line.x1, p.x1), y0: Math.min(line.y0, p.y0), y1: Math.max(line.y1, p.y1) });
		else lines.push({ ...p });
	}
	return lines;
}

async function pageRegions(page: PdfPage): Promise<Region[]> {
	const viewport = page.getViewport({ scale: 1 });
	const { items } = await page.getTextContent();
	const lines = items
		// Rotated text is a margin stamp such as arXiv's, not part of the columns.
		.filter(i => i.str.trim() && i.transform[1] === 0 && i.transform[2] === 0)
		.map((i) => {
			const [e, f] = [i.transform[4], i.transform[5]];
			const [ax, ay, bx, by] = viewport.convertToViewportRectangle([e, f, e + i.width, f + i.height]);
			return {
				x0: Math.min(ax, bx) / viewport.width, x1: Math.max(ax, bx) / viewport.width,
				y0: Math.min(ay, by) / viewport.height, y1: Math.max(ay, by) / viewport.height,
			};
		});
	return regions(lines);
}

/**
 * Column mode, for two-column PDFs: each column is zoomed to the screen's width and read a screen
 * at a time, left column, then right, with parts spanning both columns shown whole in between;
 * turning the page steps through them. Remembered per document. Must run before the reader is
 * created, which renders the toolbar button; returns the hook that takes the reader and returns
 * the toggle.
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
	const win = () => view._iframeWindow;
	const viewer = () => win()?.PDFViewerApplication.pdfViewer;
	const cache = new Map<number, Promise<Region[]>>();
	const regionsOf = (page: number) => {
		if (!cache.has(page)) {
			const doc = win()?.PDFViewerApplication.pdfDocument;
			if (!doc) return Promise.resolve([{ x0: 0, x1: 1, y0: 0, y1: 1 }]);
			cache.set(page, doc.getPage(page + 1).then(pageRegions));
		}
		return cache.get(page)!;
	};
	/** Where reading is: the page, its region, and the step down that region. */
	let at = { page: -1, region: 0, step: 0 };
	/** Where `show` last scrolled to; anywhere else, the reader was scrolled by hand. */
	let shown = { left: NaN, top: NaN };
	let scaleBefore = 'auto';

	/** The page width (CSS px) that fits `r` to the screen, and the tops of the screens down it. */
	const fit = (v: PdfViewer, div: HTMLElement, r: Region) => {
		const c = v.container;
		const width = c.clientWidth / (r.x1 - r.x0);
		const screen = c.clientHeight / (width * div.clientHeight / div.clientWidth);
		const tops = [r.y0];
		while (tops[tops.length - 1] + screen < r.y1) tops.push(tops[tops.length - 1] + screen * (1 - OVERLAP));
		return { width, tops };
	};

	const show = (v: PdfViewer, page: number, rs: Region[], region: number, step: number) => {
		const div = v._pages[page]?.div;
		region = region < 0 ? rs.length - 1 : region;
		const r = rs[region];
		if (!div || !r) return;
		const { width, tops } = fit(v, div, r);
		step = step < 0 ? tops.length - 1 : Math.min(step, tops.length - 1);
		at = { page, region, step };
		// Re-rendering at a new zoom is slow, and flashes on e-ink; neighbouring columns differ by a few pixels.
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

	let queue = Promise.resolve();
	const turn = (direction: 1 | -1, original: () => void) => () => {
		const v = viewer();
		if (!on || !v) return original();
		const forward = direction > 0;
		// Swipes can come faster than a page's text is read.
		queue = queue.then(async () => {
			let { page, region, step } = at;
			// Scrolled elsewhere by hand: start that page over. A region's last screen can show the next
			// page, which pdf.js then counts as the current one.
			if (Math.abs(v.container.scrollTop - shown.top) > 2 || Math.abs(v.container.scrollLeft - shown.left) > 2) {
				page = v.currentPageNumber - 1;
				return show(v, page, await regionsOf(page), forward ? 0 : -1, forward ? 0 : -1);
			}
			const rs = await regionsOf(page);
			const div = v._pages[page]?.div;
			const steps = div && rs[region] ? fit(v, div, rs[region]).tops.length : 1;
			step += direction;
			if (step >= 0 && step < steps) return show(v, page, rs, region, step);
			region += direction;
			if (region >= 0 && region < rs.length) return show(v, page, rs, region, forward ? 0 : -1);
			page += direction;
			if (page < 0 || page >= v._pages.length) return;
			const next = await regionsOf(page);
			v.currentPageNumber = page + 1;
			show(v, page, next, forward ? 0 : -1, forward ? 0 : -1);
		}).catch(console.error);
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
		// Lets the pen code take touches, so swipes turn instead of scrolling.
		view.setTool(view._tool);
		if (!on) {
			v.currentScaleValue = scaleBefore;
			return;
		}
		scaleBefore = v.currentScaleValue;
		const page = v.currentPageNumber - 1;
		// Back from a look at the whole page, e.g. at a figure: carry on where reading stopped.
		const resume = at.page === page ? at : { page, region: 0, step: 0 };
		queue = queue.then(async () => show(v, page, await regionsOf(page), resume.region, resume.step)).catch(console.error);
	};

	button.addEventListener('click', () => set(!on));
	// A rotated phone or a resized window fits the region again, once the reader has re-applied its own zoom.
	let refit = 0;
	window.addEventListener('resize', () => {
		clearTimeout(refit);
		refit = window.setTimeout(() => {
			const v = viewer();
			if (!on || !v || at.page < 0) return;
			const { page, region, step } = at;
			queue = queue.then(async () => show(v, page, await regionsOf(page), region, step)).catch(console.error);
		}, 300);
	});

	view.initializedPromise.then(() => {
		let saved = false;
		try { saved = localStorage.getItem(key) === '1'; }
		catch { /* remembering is a convenience */ }
		if (saved) set(true);
	});
	return () => set(!on);
}
