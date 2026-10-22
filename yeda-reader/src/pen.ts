import { columnMode } from './columns';

/** A pointer event as the PDF view's handlers take it. */
type Handler = (event: PointerEvent) => void;

interface PdfViewer {
	container: HTMLElement;
	currentPageNumber: number;
	_pages: { div: HTMLElement }[];
	currentScale: number;
	currentScaleValue: string;
	scrollMode: number;
}

interface View {
	/** Resolves once the document is shown with its saved zoom and scroll mode applied. */
	initializedPromise: Promise<unknown>;
	_iframe: HTMLIFrameElement;
	_tool: { type: string };
	_iframeWindow: (Window & { PDFViewerApplication: { pdfViewer: PdfViewer } }) | null;
	setTool(tool: { type: string }): void;
	navigateToNextPage(): void;
	navigateToPreviousPage(): void;
	/** Redraws every page's annotation layer. */
	_render(): void;
	/** The gesture in progress; an ink stroke knows its page once it has started. */
	action: { annotation?: { position: { pageIndex: number } } } | null;
	_handlePointerDown: Handler;
	_handlePointerMove: Handler;
	_handlePointerUp: Handler;
}

export interface PenReader {
	_primaryView: View | null;
	_tools: Record<string, { type: string; color?: string; size?: number }>;
	_state: { tool: { type: string } };
	setTool(params: { type: string }): void;
	navigateToNextPage(): void;
	navigateToPreviousPage(): void;
}

const EINK_KEY = 'yeda.eink';
/** Touches this soon after the pen lifts are the hand resting, not a gesture. */
const PALM_AFTER_PEN_MS = 600;
/** A contact this large (CSS px²) is a palm; a fingertip is a few hundred. */
const PALM_AREA = 5000;
/** A finger moving this far (CSS px) is a swipe, not a tap. */
const SWIPE = 60;
/** The reader's eraser width, and the one a pen gets: at 16 a quick pass misses most of a stroke. */
const DEFAULT_ERASER = 16;
const PEN_ERASER = 40;
/** While erasing, the most screen pixels between two checked points. */
const ERASE_STEP = 6;
/** pdf.js ScrollMode.PAGE: one page at a time. */
const PAGE_SCROLL_MODE = 3;

export const einkMode = () => localStorage.getItem(EINK_KEY) === '1';
export const setEinkMode = (on: boolean) => localStorage.setItem(EINK_KEY, on ? '1' : '0');

/** Side button (2) or eraser end (32 / button 5); the AiPaper's pen reports both as the eraser. */
const erasing = (e: PointerEvent) => (e.buttons & 34) !== 0 || e.button === 5;

const NO_MOTION = '*, *::before, *::after { transition: none !important; animation: none !important; scroll-behavior: auto !important; }';
const BIG_TARGETS = '.toolbar button, .toolbar .toolbar-button { min-width: 48px; min-height: 48px; }';

function addStyle(doc: Document, css: string) {
	doc.head.append(Object.assign(doc.createElement('style'), { textContent: css }));
}

/**
 * Pen tablets: once a pen has been used, it draws while a finger pans and zooms instead, a
 * resting palm is ignored, and the pen's eraser button erases. With e-ink mode on, pages turn
 * instead of scrolling and nothing animates. Wraps the PDF view's handlers, which it binds once
 * its iframe has loaded, so it must run right after the reader is created.
 */
export function installPen(reader: PenReader) {
	const view = reader._primaryView;
	if (!view) return;
	const eink = einkMode();
	if (eink) {
		addStyle(document, NO_MOTION + BIG_TARGETS);
		// Grey and yellow ink vanish on a black-and-white screen.
		if (reader._tools.ink) reader._tools.ink.color = '#000000';
	}

	const setTool = view.setTool.bind(view);
	// The reader lets the browser scroll natively under the pointer tool; e-ink and column mode handle touch themselves.
	view.setTool = (tool) => {
		setTool(tool);
		const container = view._iframeWindow?.document.getElementById('viewerContainer');
		if (container && (eink || columnMode())) container.style.touchAction = 'none';
	};

	const down = view._handlePointerDown.bind(view);
	const move = view._handlePointerMove;
	const up = view._handlePointerUp.bind(view);
	let penSeen = false;
	let penDown = false;
	let penUpAt = 0;
	let lastPen: PointerEvent | null = null;
	/**
	 * The ink stroke being drawn, on a canvas of its own in the page. The reader re-serializes and
	 * redraws the whole page, stroke included, on every move, which takes most of a frame on the
	 * AiPaper and grows with the stroke; it redraws once, when the pen lifts.
	 */
	let stroke: { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D; page: HTMLElement; render: () => void } | null = null;

	const startStroke = (e: PointerEvent) => {
		const v = viewer();
		const win = view._iframeWindow;
		const pageIndex = view.action?.annotation?.position.pageIndex;
		const page = pageIndex === undefined ? undefined : v?._pages[pageIndex]?.div;
		const ink = reader._tools.ink;
		if (!v || !win || !page || !ink) return;
		const canvas = win.document.createElement('canvas');
		const dpr = win.devicePixelRatio;
		Object.assign(canvas.style, { position: 'absolute', left: '0', top: '0', width: '100%', height: '100%', pointerEvents: 'none', zIndex: '100' });
		canvas.width = page.clientWidth * dpr;
		canvas.height = page.clientHeight * dpr;
		page.append(canvas);
		const ctx = canvas.getContext('2d')!;
		ctx.scale(dpr, dpr);
		ctx.strokeStyle = ink.color ?? '#000000';
		// The tool's size is in PDF points, which pdf.js draws at 96/72 CSS pixels per point at scale 1.
		ctx.lineWidth = (ink.size ?? 2) * v.currentScale * 96 / 72;
		ctx.lineCap = ctx.lineJoin = 'round';
		const r = page.getBoundingClientRect();
		ctx.beginPath();
		ctx.moveTo(e.clientX - r.left, e.clientY - r.top);
		stroke = { canvas, ctx, page, render: view._render };
		view._render = () => {};
	};

	const endStroke = () => {
		if (!stroke) return;
		const { canvas, render } = stroke;
		view._render = render;
		stroke = null;
		// Until the reader's own ink has been painted.
		requestAnimationFrame(() => requestAnimationFrame(() => canvas.remove()));
	};

	/** The tool to go back to after a stroke with the eraser button. */
	let restoreTool: string | null = null;
	const touches = new Map<number, { x: number; y: number; start: PointerEvent }>();
	let pinch = 0;
	/** The gesture moved the view, so lifting the finger is not a tap or a swipe. */
	let moved = false;

	const viewer = () => view._iframeWindow?.PDFViewerApplication.pdfViewer;
	const drawing = () => !['pointer', 'hand'].includes(view._tool.type);
	// E-ink mode takes every touch: swipes turn pages, and native scrolling would smear the screen.
	// Column mode does too: swipes step through the columns.
	const turns = () => eink || columnMode();
	const ours = (e: PointerEvent) => e.pointerType === 'touch' && (turns() || (penSeen && drawing()));
	// The page itself, not the container: pdf.js pads the container, so it scrolls a little even at page-fit.
	const zoomed = (v: PdfViewer) => {
		const page = v._pages[v.currentPageNumber - 1]?.div;
		return !!page && (page.offsetWidth > v.container.clientWidth + 2 || page.offsetHeight > v.container.clientHeight + 2);
	};
	const palm = (e: PointerEvent) => penDown || performance.now() - penUpAt < PALM_AFTER_PEN_MS || e.width * e.height > PALM_AREA;
	const spread = () => {
		const [a, b] = [...touches.values()];
		return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
	};

	view._handlePointerDown = (e) => {
		if (e.pointerType === 'pen') {
			// A cancelled stroke never sees its pointerup.
			endStroke();
			if (!penSeen && reader._tools.eraser?.size === DEFAULT_ERASER) reader._tools.eraser.size = PEN_ERASER;
			penSeen = penDown = true;
			lastPen = e;
			if (erasing(e) && view._tool.type !== 'eraser') {
				restoreTool = reader._state.tool.type;
				reader.setTool({ type: 'eraser' });
				// The reader passes its tool to the view on its next render, after this stroke began.
				view.setTool(reader._tools.eraser);
			}
		}
		else if (ours(e)) {
			if (!palm(e)) {
				if (touches.size === 0) moved = false;
				touches.set(e.pointerId, { x: e.clientX, y: e.clientY, start: e });
				pinch = touches.size === 2 ? spread() : 0;
			}
			return;
		}
		down(e);
		if (e.pointerType === 'pen' && view.action?.annotation && view._tool.type === 'ink') startStroke(e);
	};

	view._handlePointerMove = (e) => {
		const t = touches.get(e.pointerId);
		if (t) {
			const [dx, dy] = [e.clientX - t.x, e.clientY - t.y];
			t.x = e.clientX;
			t.y = e.clientY;
			const v = viewer();
			if (!v) return;
			if (touches.size === 2) {
				const now = spread();
				// Re-rendering at every small step would flash an e-ink screen.
				if (pinch && Math.abs(now / pinch - 1) > 0.08) {
					v.currentScale *= now / pinch;
					pinch = now;
					moved = true;
				}
			}
			// On e-ink a page that fits turns instead; only a zoomed-in one pans. Columns always turn.
			else if (!columnMode() && (!eink || zoomed(v))) {
				v.container.scrollBy(-dx, -dy);
				moved ||= Math.hypot(e.clientX - t.start.clientX, e.clientY - t.start.clientY) > 8;
			}
			return;
		}
		if (ours(e)) return;
		// The WebView delivers pen moves in batches; every point in between makes the stroke smooth.
		if (e.pointerType === 'pen' && e.buttons && ['ink', 'eraser'].includes(view._tool.type)) {
			const points = e.getCoalescedEvents?.() ?? [];
			// Each move redraws every page; one redraw, after the batch's last point, is enough.
			const render = view._render;
			view._render = () => {};
			const page = stroke?.page.getBoundingClientRect();
			try {
				for (const p of [...points.slice(0, -1), e]) {
					// The eraser only removes ink within its circle, so a fast pass needs points in between.
					if (view._tool.type === 'eraser' && lastPen) {
						const steps = Math.floor(Math.hypot(p.clientX - lastPen.clientX, p.clientY - lastPen.clientY) / ERASE_STEP);
						for (let i = 1; i < steps; i++) {
							const f = i / steps;
							move(new PointerEvent('pointermove', {
								pointerId: p.pointerId, pointerType: 'pen', buttons: p.buttons,
								clientX: lastPen.clientX + (p.clientX - lastPen.clientX) * f,
								clientY: lastPen.clientY + (p.clientY - lastPen.clientY) * f,
							}));
						}
					}
					if (p === e && !stroke) view._render = render;
					move(p);
					lastPen = p;
					if (stroke && page) stroke.ctx.lineTo(p.clientX - page.left, p.clientY - page.top);
				}
				if (stroke && page) {
					const { ctx } = stroke;
					ctx.stroke();
					ctx.beginPath();
					ctx.moveTo(e.clientX - page.left, e.clientY - page.top);
				}
			}
			finally {
				if (!stroke) view._render = render;
			}
			return;
		}
		move(e);
	};

	view._handlePointerUp = (e) => {
		const t = touches.get(e.pointerId);
		if (t) {
			const single = touches.size === 1;
			touches.delete(e.pointerId);
			const [dx, dy] = [e.clientX - t.start.clientX, e.clientY - t.start.clientY];
			const dominant = Math.abs(dx) > Math.abs(dy) ? dx : dy;
			if (single && !moved) {
				if (turns() && Math.abs(dominant) > SWIPE) {
					if (dominant < 0) reader.navigateToNextPage();
					else reader.navigateToPreviousPage();
				}
				// A tap still selects and follows links, as long as it can't leave ink.
				else if (Math.abs(dominant) <= SWIPE && !drawing()) {
					down(t.start);
					up(e);
				}
			}
			if (touches.size < 2) pinch = 0;
			return;
		}
		if (ours(e)) return;
		if (e.pointerType === 'pen' && stroke) view._render = stroke.render;
		up(e);
		if (e.pointerType === 'pen') {
			endStroke();
			penDown = false;
			penUpAt = performance.now();
			if (restoreTool) {
				reader.setTool({ type: restoreTool });
				view.setTool(reader._tools[restoreTool]);
				restoreTool = null;
			}
		}
	};
}

/** In e-ink mode, once the document is shown: one page at a time, fitted, and no animation. */
export async function settleEink(reader: PenReader) {
	const view = reader._primaryView;
	if (!einkMode() || !view) return;
	// Earlier, the view's own saved zoom and scroll mode would replace these.
	await view.initializedPromise;
	const win = view._iframeWindow;
	if (!win) return;
	addStyle(win.document, NO_MOTION);
	const container = win.document.getElementById('viewerContainer');
	if (container) container.style.touchAction = 'none';
	const v = win.PDFViewerApplication.pdfViewer;
	v.scrollMode = PAGE_SCROLL_MODE;
	v.currentScaleValue = 'page-fit';
}
