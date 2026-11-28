import type { VimReader } from './vim';

type Rect = [number, number, number, number];
type XY = [number, number];
type Point = { page: number; offset: number };
type Mode = 'off' | 'caret' | 'visual' | 'area';
type Motion = 'h' | 'j' | 'k' | 'l' | 'w' | 'b' | 'e' | '0' | '$';
type Direction = -1 | 1;

interface Char {
	rect: Rect;
	inlineRect?: Rect;
	wordBreakAfter?: boolean;
	spaceAfter?: boolean;
	lineBreakAfter?: boolean;
}

interface PageData {
	chars: Char[];
	viewBox: Rect;
}

interface Position {
	pageIndex: number;
	rects: Rect[];
	nextPageRects?: Rect[];
	fontSize?: number;
	rotation?: number;
}

interface SelectionRange {
	anchor?: boolean;
	head?: boolean;
	collapsed?: boolean;
	anchorOffset?: number;
	headOffset?: number;
	position: { pageIndex: number; rects?: Rect[]; nextPageRects?: Rect[] };
	text?: string;
}

interface AnnotationMeta {
	pageLabel: string;
	sortIndex: string;
}

interface AnnotationDraft extends AnnotationMeta {
	type: string;
	color?: string;
	position: Position;
	text?: string;
}

export interface FindState {
	active: boolean;
	result?: { annotation?: { position: Position } } | null;
}

export interface PdfView {
	initializedPromise: Promise<unknown>;
	_iframeWindow: (Window & {
		PDFViewerApplication: { pdfViewer: { currentPageNumber: number; pagesCount: number; scrollMode: number } };
	}) | null;
	_pdfPages: Record<number, PageData | undefined>;
	_selectionRanges: SelectionRange[];
	_highlightedPosition: Position | null;
	_ensureBasicPageData(page: number): Promise<PageData | null>;
	_syncSelectionAfterSemanticChange(): void;
	_onSetSelectionPopup(popup?: unknown): void;
	_getAnnotationFromSelectionRanges(ranges: SelectionRange[], type: string, color?: string): AnnotationDraft | null;
	_onAddAnnotation(annotation: AnnotationDraft, select: boolean): { id: string } | null | undefined;
	_focusTextAnnotation(id: string): boolean;
	_handleKeyDown(event: KeyboardEvent): void;
	_render(pageIndexes?: number[]): void;
	clearSelection(): void;
	getAnnotationMeta(position: Position): AnnotationMeta;
	navigateToPosition(position: Position, options: { block: 'nearest'; inline: 'nearest'; ifNeeded: true; behavior: 'instant' }): void;
}

type Area = { page: number; bounds: Rect; fixed: XY; free: XY };
type Current = () => boolean;
type Command = (reader: VimReader, view: PdfView, current: Current) => void | Promise<void>;

const AREA_HELP = '-- AREA: hjkl corner, HJKL ×5, o other corner, Enter saves --';
const MIN_IMAGE_SIZE = 10;
const NOTE_SIZE = 22;
const PAGE_SCROLL_MODE = 3;
const NO_MODIFIERS = { shiftKey: false, ctrlKey: false, metaKey: false };

const viewer = (view: PdfView) => view._iframeWindow?.PDFViewerApplication?.pdfViewer;
const wordEnd = (c: Char) => c.wordBreakAfter || c.spaceAfter || c.lineBreakAfter;
const centerX = (rect: Rect) => (rect[0] + rect[2]) / 2;
const position = (pageIndex: number, rect: Rect): Position => ({ pageIndex, rects: [rect] });
const clamp = (x: number, lo: number, hi: number) => Math.max(lo, Math.min(x, hi));
const clampPoint = (x: number, y: number, rect: Rect): XY => [clamp(x, rect[0], rect[2]), clamp(y, rect[1], rect[3])];
const areaRect = ({ fixed: [x, y], free: [u, v] }: Area): Rect => [Math.min(x, u), Math.min(y, v), Math.max(x, u), Math.max(y, v)];

function lineStart(chars: Char[], offset: number) {
	while (offset > 0 && !chars[offset - 1].lineBreakAfter) offset--;
	return offset;
}

function lineEnd(chars: Char[], offset: number) {
	while (offset + 1 < chars.length && !chars[offset].lineBreakAfter) offset++;
	return offset;
}

function nearestX(chars: Char[], first: number, last: number, x: number) {
	let offset = first;
	let distance = Math.abs(centerX(chars[offset].rect) - x);
	for (let i = first + 1; i <= last; i++) {
		const next = Math.abs(centerX(chars[i].rect) - x);
		if (next < distance) {
			offset = i;
			distance = next;
		}
	}
	return offset;
}

export function createCursor(getReader: () => VimReader | undefined, editComment: (r: VimReader) => void) {
	let owner: PdfView | undefined;
	let mode: Mode = 'off';
	let requested: Mode = 'off';
	let previousAreaMode: Exclude<Mode, 'area'> = 'off';
	let runningAction = false;
	let head: Point | null = null;
	let anchor: Point | null = null;
	let wantX: number | null = null;
	let viewportPage: number | undefined;
	let area: Area | null = null;
	let indicator: HTMLDivElement | undefined;
	let flashTimer: number | undefined;
	let generation = 0;
	let pending: Promise<void> | undefined;
	const hooked = new WeakSet<PdfView>();
	const selectionPages = new Map<number, PageData>();

	function showMode(message?: string) {
		window.clearTimeout(flashTimer);
		if (mode === 'off') {
			if (indicator) indicator.hidden = true;
			return;
		}
		if (!indicator) {
			indicator = document.createElement('div');
			indicator.setAttribute('role', 'status');
			indicator.style.cssText = 'position:fixed;left:8px;bottom:8px;z-index:10000;padding:2px 6px;background:Canvas;color:CanvasText;border:1px solid GrayText;border-radius:4px;font:12px monospace;pointer-events:none';
			document.body.append(indicator);
		}
		indicator.hidden = false;
		indicator.textContent = message ?? (mode === 'area' ? AREA_HELP : `-- ${mode.toUpperCase()} --`);
	}

	function cancelPending() {
		generation++;
		pending = undefined;
	}

	function currentView() {
		const view = getReader()?._primaryView;
		if (owner && owner !== view) {
			exit();
			owner = undefined;
		}
		return view;
	}

	// Publish the requested mode to the keymap immediately, but apply commands in input order.
	// Escape and pointerdown discard this queue without waiting for old PDF page loads.
	function enqueue(command: Command) {
		const reader = getReader();
		const view = currentView();
		if (!reader || !view) return;
		owner = view;
		install(view);
		const epoch = generation;
		const current = () => epoch === generation && getReader()?._primaryView === view;
		const run = () => {
			if (current()) return command(reader, view, current);
		};
		const failed = (error: unknown) => {
			if (current()) {
				exit();
				console.error('PDF keyboard cursor:', error);
			}
		};
		try {
			const result = pending ? pending.then(run) : run();
			if (!result || !current()) return;
			const tail = result.catch(failed);
			pending = tail;
			void tail.then(() => {
				if (pending === tail) pending = undefined;
			});
		} catch (error) {
			failed(error);
		}
	}

	function charAt(view: PdfView, point: Point) {
		return view._pdfPages[point.page]?.chars[point.offset] ?? selectionPages.get(point.page)?.chars[point.offset];
	}

	function install(view: PdfView) {
		const win = view._iframeWindow;
		if (!win || hooked.has(view)) return;
		hooked.add(view);
		const render = view._render.bind(view);
		view._render = (pageIndexes) => {
			if (owner === view && mode !== 'off') {
				const c = head && charAt(view, head);
				view._highlightedPosition = mode === 'area' && area
					? position(area.page, areaRect(area))
					: mode !== 'area' && head && c ? position(head.page, c.inlineRect ?? c.rect) : null;
			}
			render(pageIndexes);
		};
		const clear = view.clearSelection.bind(view);
		view.clearSelection = () => {
			clear();
			if (owner === view && mode === 'visual') {
				cancelPending();
				requested = 'caret';
				toCaret(view, false);
			}
		};
		const sync = view._syncSelectionAfterSemanticChange.bind(view);
		view._syncSelectionAfterSemanticChange = () => {
			if (owner !== view || mode !== 'visual' || !head || !anchor) {
				sync();
				return;
			}
			const { page: ap, offset: ao } = anchor;
			const { page: hp, offset: ho } = head;
			const forward = ap < hp || ap === hp && ao <= ho;
			const anchorOffset = ao + Number(!forward);
			const headOffset = ho + Number(forward);
			view._selectionRanges = ap === hp
				? [{ anchor: true, head: true, collapsed: false, anchorOffset, headOffset, position: { pageIndex: ap } }]
				: [
					{ anchor: true, collapsed: false, anchorOffset, position: { pageIndex: ap } },
					{ head: true, collapsed: false, headOffset, position: { pageIndex: hp } },
				];
			const popup = view._onSetSelectionPopup;
			const pages = view._pdfPages;
			let selection = pages;
			for (let i = Math.min(ap, hp); i <= Math.max(ap, hp); i++) {
				// The reader skips absent pages but aborts at empty ones. Keep held data for
				// pages evicted by pdf.js and omit textless pages from the range calculation.
				const page = pages[i]?.chars.length ? pages[i] : selectionPages.get(i);
				const data = page?.chars.length ? page : undefined;
				if (data !== pages[i]) {
					if (selection === pages) selection = Object.create(pages) as typeof pages;
					selection[i] = data;
				}
			}
			view._onSetSelectionPopup = () => {};
			view._pdfPages = selection;
			try {
				sync();
				// Preparing a mouse popup attaches continuation rects to the first range.
				// Keyboard ranges are drawn separately and may skip pages with no text.
				if (view._selectionRanges[0]) delete view._selectionRanges[0].position.nextPageRects;
			} finally {
				view._pdfPages = pages;
				view._onSetSelectionPopup = popup;
			}
		};
		win.addEventListener('pointerdown', () => {
			if (owner === view) exit();
		}, true);
	}

	async function pageData(view: PdfView, page: number): Promise<PageData | undefined> {
		const data = await view._ensureBasicPageData(page);
		return view._pdfPages[page] ?? data ?? undefined;
	}

	async function findPage(view: PdfView, page: number, dir: Direction, current: Current): Promise<{ page: number; chars: Char[] } | undefined> {
		const count = viewer(view)?.pagesCount ?? 0;
		for (; page >= 0 && page < count; page += dir) {
			const chars = (await pageData(view, page))?.chars;
			if (!current()) return;
			if (chars?.length) return { page, chars };
		}
	}

	async function headForMotion(view: PdfView, current: Current) {
		const pdf = viewer(view);
		if (!pdf?.pagesCount) return;
		const page = pdf.currentPageNumber - 1;
		const start = head && (head.page === page || viewportPage === page) ? head : { page, offset: 0 };
		const chars = view._pdfPages[start.page]?.chars ?? selectionPages.get(start.page)?.chars;
		if (chars?.[start.offset]) return { point: start, chars };
		const found = await findPage(view, start.page, 1, current);
		if (found) {
			return {
				point: { page: found.page, offset: found.page === start.page ? Math.min(start.offset, found.chars.length - 1) : 0 },
				chars: found.chars,
			};
		}
	}

	async function paintHead(view: PdfView, current: Current, next = head, pivot = anchor, column = wantX) {
		pivot = mode === 'visual' ? pivot ?? next : null;
		if (next && pivot) {
			const first = Math.min(next.page, pivot.page);
			const last = Math.max(next.page, pivot.page);
			for (let p = first; p <= last; p++) {
				const data = view._pdfPages[p]?.chars.length ? view._pdfPages[p] : selectionPages.get(p) ?? await pageData(view, p);
				if (!current()) return;
				if (data) selectionPages.set(p, data);
			}
		}
		if (!current()) return;
		head = next;
		anchor = pivot;
		wantX = column;
		const c = head && charAt(view, head);
		const pdf = viewer(view);
		if (head && c && pdf) {
			// PDF.js detaches non-current pages in page scroll mode.
			if (pdf.scrollMode === PAGE_SCROLL_MODE && pdf.currentPageNumber !== head.page + 1) pdf.currentPageNumber = head.page + 1;
			view.navigateToPosition(position(head.page, c.rect), { block: 'nearest', inline: 'nearest', ifNeeded: true, behavior: 'instant' });
			viewportPage = pdf.currentPageNumber - 1;
		}
		if (mode === 'visual') {
			if (head && anchor) view._syncSelectionAfterSemanticChange();
			else view._selectionRanges = [];
		}
		view._render();
	}

	function clearRanges(view: PdfView) {
		view.clearSelection();
		view._selectionRanges = [];
	}

	function toCaret(view: PdfView, clear = true) {
		mode = 'caret';
		anchor = null;
		selectionPages.clear();
		if (clear) clearRanges(view);
		else view._selectionRanges = [];
		view._render();
		showMode();
	}

	function exit() {
		cancelPending();
		const wasActive = requested !== 'off' || mode !== 'off';
		mode = requested = 'off';
		area = head = anchor = wantX = null;
		viewportPage = undefined;
		selectionPages.clear();
		if (wasActive && owner?._iframeWindow) {
			clearRanges(owner);
			owner._highlightedPosition = null;
			owner._render();
		}
		showMode();
	}

	async function enterCaret(reader: VimReader, view: PdfView, current: Current) {
		mode = 'caret';
		showMode();
		const ready = await view.initializedPromise;
		if (!current()) return;
		if (ready === false) { exit(); return; }
		install(view);
		reader.setSelectedAnnotations([], false, NO_MODIFIERS);
		clearRanges(view);
		const start = await headForMotion(view, current);
		if (current()) await paintHead(view, current, start?.point ?? null, null, null);
	}

	function toggleVisual() {
		if (!currentView() || requested === 'area') return;
		const from = requested;
		requested = from === 'caret' ? 'visual' : 'caret';
		enqueue(async (reader, view, current) => {
			if (from === 'off') {
				await enterCaret(reader, view, current);
			} else if (from === 'caret') {
				mode = 'visual';
				showMode();
				await paintHead(view, current);
			} else {
				// The requested mode can already include another queued `v`.
				toCaret(view);
			}
		});
	}

	function escape(): boolean {
		const view = currentView();
		if (!view || requested === 'off') return false;
		const from = runningAction ? mode : requested;
		const back = from === 'area' ? previousAreaMode : from === 'visual' ? 'caret' : 'off';
		if (back === 'off') exit();
		else {
			cancelPending();
			area = null;
			requested = back;
			if (back === 'caret' && view._iframeWindow) toCaret(view);
			else {
				mode = back;
				showMode();
			}
			if (!head || back === 'visual') {
				enqueue(async (reader, view, current) => {
					if (!head) await enterCaret(reader, view, current);
					if (!current() || back !== 'visual') return;
					mode = 'visual';
					showMode();
					await paintHead(view, current);
				});
			}
		}
		return true;
	}

	function move(motion: Motion, count: number) {
		enqueue(async (_reader, view, current) => {
			if (mode !== 'caret' && mode !== 'visual') return;
			const start = await headForMotion(view, current);
			if (!current()) return;
			if (!start) { await paintHead(view, current, null, anchor, null); return; }
			let { page, offset } = start.point;
			let chars = start.chars;
			const vertical = motion === 'j' || motion === 'k';
			const column = vertical ? (start.point === head ? wantX : null) ?? centerX(chars[offset].rect) : null;
			const dir = motion === 'h' || motion === 'b' || motion === 'k' ? -1 : 1;
			const repeats = motion === '0' || motion === '$' ? 1 : count;
			for (let i = 0; i < repeats; i++) {
				let next = offset;
				switch (motion) {
					case 'h': case 'l': next += dir; break;
					case '0': next = lineStart(chars, offset); break;
					case '$': next = lineEnd(chars, offset); break;
					case 'w':
						do { next++; } while (next < chars.length && !wordEnd(chars[next - 1]));
						break;
					case 'b':
						do { next--; } while (next > 0 && !wordEnd(chars[next - 1]));
						break;
					case 'e':
						do { next++; } while (next + 1 < chars.length && !wordEnd(chars[next]));
						break;
					case 'j': next = lineEnd(chars, offset) + 1; break;
					case 'k': next = lineStart(chars, offset) - 1; break;
				}
				if (next < 0 || next >= chars.length) {
					const found = await findPage(view, page + dir, dir, current);
					if (!current()) return;
					if (!found) break;
					({ page, chars } = found);
					next = dir > 0 ? 0 : chars.length - 1;
					if (motion === 'b') while (next > 0 && !wordEnd(chars[next - 1])) next--;
					if (motion === 'e') while (next + 1 < chars.length && !wordEnd(chars[next])) next++;
				}
				if (column !== null) next = nearestX(chars, lineStart(chars, next), lineEnd(chars, next), column);
				offset = next;
			}
			await paintHead(view, current, { page, offset }, anchor ?? start.point, column);
		});
	}

	function searchMove(dir: Direction, count = 1) {
		enqueue(async (reader, view, current) => {
			for (let i = 0; i < count; i++) {
				const before = reader._state.primaryViewFindState;
				const previous = before.result;
				if (dir > 0) reader.findNext();
				else reader.findPrevious();
				if (!before.active || mode !== 'caret' && mode !== 'visual') return;
				let match: Position | undefined;
				for (let frame = 0; frame <= 20; frame++) {
					const find = reader._state.primaryViewFindState;
					if (!find.active) return;
					if (find.result !== previous) match = find.result?.annotation?.position;
					if (match || frame === 20) break;
					const { promise, resolve } = Promise.withResolvers<void>();
					requestAnimationFrame(() => resolve());
					await promise;
					if (!current()) return;
				}
				if (!match) continue;
				const visual = mode === 'visual';
				const across = visual && !!match.nextPageRects?.length;
				const page = match.pageIndex + Number(across);
				const rect = visual ? (across ? match.nextPageRects : match.rects)?.at(-1) : match.rects[0];
				if (!rect) continue;
				const chars = (await pageData(view, page))?.chars;
				if (!current()) return;
				if (!chars) continue;
				const within = (c: Char) => {
					const x = centerX(c.rect), y = (c.rect[1] + c.rect[3]) / 2;
					return x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3];
				};
				const offset = visual ? chars.findLastIndex(within) : chars.findIndex(within);
				if (offset < 0) continue;
				await paintHead(view, current, { page, offset }, anchor, null);
				if (!current()) return;
			}
		});
	}

	function swap() {
		enqueue(async (_reader, view, current) => {
			if (mode !== 'visual' || !head || !anchor) return;
			await paintHead(view, current, anchor, head, null);
		});
	}

	function place(type: 'note' | 'text') {
		const reader = getReader();
		const view = reader?._primaryView;
		if (!reader || !view || reader._state.readOnly || mode !== 'caret' || !head) return;
		const c = charAt(view, head);
		if (!c) return;
		const tool = reader._tools[type];
		const size = type === 'note' ? NOTE_SIZE : tool.size;
		if (size === undefined) return;
		const x = centerX(c.rect), y = (c.rect[1] + c.rect[3]) / 2, half = size / 2;
		const pos = position(head.page, [x - half, y - half, x + half, y + half]);
		if (type === 'text') Object.assign(pos, { fontSize: size, rotation: 0 });
		const added = view._onAddAnnotation({ type, color: tool.color, position: pos, ...view.getAnnotationMeta(pos) }, true);
		if (!added) return;
		exit();
		if (type === 'note') editComment(reader);
		else {
			const epoch = generation;
			window.setTimeout(() => {
				if (epoch === generation && requested === 'off' && getReader()?._primaryView === view
					&& reader._state.selectedAnnotationIDs.includes(added.id)) view._focusTextAnnotation(added.id);
			}, 100);
		}
	}

	function startArea() {
		const reader = getReader();
		if (!currentView() || !reader || reader._state.readOnly || requested === 'area') return;
		previousAreaMode = requested;
		requested = 'area';
		enqueue(async (r, view, current) => {
			const ready = await view.initializedPromise;
			if (!current()) return;
			if (ready === false) { exit(); return; }
			install(view);
			const c = head && charAt(view, head);
			const page = c && head ? head.page : (viewer(view)?.currentPageNumber ?? 0) - 1;
			if (page < 0) { exit(); return; }
			const data = await pageData(view, page);
			if (!current()) return;
			if (!data || r._state.readOnly) { exit(); return; }
			mode = 'area';
			r.setSelectedAnnotations([], false, NO_MODIFIERS);
			clearRanges(view);
			const bounds = data.viewBox;
			const fixed = c ? clampPoint(c.rect[0], c.rect[3], bounds) : clampPoint(bounds[0] + 36, bounds[3] - 36, bounds);
			const free = clampPoint(fixed[0] + 100, fixed[1] - 100, bounds);
			area = { page, bounds, fixed, free };
			showMode();
			view._render();
		});
	}

	function commitArea() {
		enqueue((reader, view) => {
			if (mode !== 'area' || !area || reader._state.readOnly) return;
			const rect = areaRect(area);
			if (rect[2] - rect[0] < MIN_IMAGE_SIZE || rect[3] - rect[1] < MIN_IMAGE_SIZE) {
				showMode('-- AREA: too small --');
				flashTimer = window.setTimeout(() => showMode(), 800);
				return;
			}
			const pos = position(area.page, rect);
			const added = view._onAddAnnotation({ type: 'image', color: reader._tools.image.color, position: pos, ...view.getAnnotationMeta(pos) }, false);
			if (!added) return;
			exit();
			// A view selection without a key would focus the empty comment instead of allowing nudges.
			reader.setSelectedAnnotations([added.id], true, { ...NO_MODIFIERS, key: 'Enter' });
		});
	}

	function areaKey(key: string, count: number): boolean {
		if (requested !== 'area') return false;
		if (key === 'Escape') return escape();
		if (key === 'Enter') { commitArea(); return true; }
		if (!['h', 'j', 'k', 'l', 'H', 'J', 'K', 'L', 'o'].includes(key)) return false;
		enqueue((_reader, view) => {
			if (mode !== 'area' || !area) return;
			if (key === 'o') [area.fixed, area.free] = [area.free, area.fixed];
			else {
				const lower = key.toLowerCase();
				const delta = count * (lower === key ? 10 : 50);
				const [x, y] = area.free;
				area.free = clampPoint(x + (lower === 'h' ? -delta : lower === 'l' ? delta : 0),
					y + (lower === 'j' ? -delta : lower === 'k' ? delta : 0), area.bounds);
			}
			showMode();
			view._render();
		});
		return true;
	}

	return {
		active: () => !!currentView() && requested !== 'off',
		mode: () => currentView() ? (runningAction ? mode : requested) : 'off',
		toggleVisual, move, searchMove, swap, exit, escape, place, startArea, areaKey, commitArea,
		text: () => owner?._selectionRanges.map(range => range.text ?? '').join(' ') ?? '',
		afterMotion(action: () => void) {
			enqueue(() => {
				runningAction = true;
				try { action(); }
				finally { runningAction = false; }
			});
		},
	};
}
