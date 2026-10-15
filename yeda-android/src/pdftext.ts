// pdf.js, from zotero/reader's build, stands in for poppler on Android: it supplies the word boxes,
// region renders and plain text that yeda-tui gets from pdftotext and pdftoppm.

interface Viewport {
	width: number;
	height: number;
	convertToViewportPoint(x: number, y: number): [number, number];
}

interface TextItem {
	str?: string;
	transform: number[];
	width: number;
	height: number;
	hasEOL?: boolean;
}

interface Page {
	getViewport(params: { scale: number }): Viewport;
	getTextContent(): Promise<{ items: TextItem[] }>;
	render(params: { canvasContext: CanvasRenderingContext2D; viewport: Viewport; transform?: number[] }): { promise: Promise<void> };
}

interface Doc {
	numPages: number;
	getPage(n: number): Promise<Page>;
	destroy(): Promise<void>;
}

interface PdfJs {
	getDocument(src: { data: Uint8Array }): { promise: Promise<Doc> };
	GlobalWorkerOptions: { workerSrc: string };
}

/** A region `pdf_image_regions` wants rendered; `rect` is in PDF coordinates, origin bottom-left. */
export interface Region {
	page: number;
	height: number;
	rect: [number, number, number, number];
}

const BUILD = new URL('pdf/build/', location.href).href;
const DPI = 150;

async function open(pdf: Uint8Array): Promise<Doc> {
	// A computed URL keeps esbuild from bundling pdf.js a second time.
	const pdfjs: PdfJs = await import(/* external */ `${BUILD}pdf.mjs`);
	pdfjs.GlobalWorkerOptions.workerSrc = `${BUILD}pdf.worker.mjs`;
	// pdf.js transfers the buffer to its worker, so give it a copy.
	return pdfjs.getDocument({ data: pdf.slice() }).promise;
}

const escape = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/** One page as `pdftotext -bbox` would print it: top-left origin, one `<word>` per line. */
async function wordBoxes(page: Page): Promise<string> {
	const viewport = page.getViewport({ scale: 1 });
	const lines = [`<page width="${viewport.width}" height="${viewport.height}">`];
	for (const item of (await page.getTextContent()).items) {
		if (!item.str?.trim()) continue;
		const [, , , , x, y] = item.transform;
		const [x0, bottom] = viewport.convertToViewportPoint(x, y);
		const [x1, top] = viewport.convertToViewportPoint(x + item.width, y + item.height);
		// ponytail: words are placed by character count within a text run; proportional fonts make the edges approximate.
		const perChar = (x1 - x0) / item.str.length;
		for (const m of item.str.matchAll(/\S+/g)) {
			const start = x0 + perChar * m.index!;
			lines.push(`<word xMin="${start}" yMin="${Math.min(top, bottom)}" xMax="${start + perChar * m[0].length}" yMax="${Math.max(top, bottom)}">${escape(m[0])}</word>`);
		}
	}
	lines.push('</page>');
	return lines.join('\n');
}

async function render(page: Page, rect: Region['rect']): Promise<number[] | null> {
	const viewport = page.getViewport({ scale: DPI / 72 });
	const [left, top] = viewport.convertToViewportPoint(rect[0], rect[3]);
	const [right, bottom] = viewport.convertToViewportPoint(rect[2], rect[1]);
	const canvas = document.createElement('canvas');
	canvas.width = Math.max(1, Math.round(right - left));
	canvas.height = Math.max(1, Math.round(bottom - top));
	const ctx = canvas.getContext('2d');
	if (!ctx) return null;
	await page.render({ canvasContext: ctx, viewport, transform: [1, 0, 0, 1, -left, -top] }).promise;
	const png = await new Promise<Blob | null>(resolve => canvas.toBlob(resolve, 'image/png'));
	return png && Array.from(new Uint8Array(await png.arrayBuffer()));
}

/** Word boxes for every page, and a PNG (or null) for each region, for `extract_annotations`. */
export async function inspect(pdf: Uint8Array, regions: Region[]) {
	const doc = await open(pdf);
	try {
		const pages: string[] = [];
		for (let n = 1; n <= doc.numPages; n++) pages.push(await wordBoxes(await doc.getPage(n)));
		const images = [];
		for (const r of regions) images.push(await render(await doc.getPage(r.page), r.rect).catch(() => null));
		return { words: pages.join('\n'), images };
	} finally {
		await doc.destroy();
	}
}

/** The document's plain text, for LLM providers that take text rather than the PDF. */
export async function plainText(pdf: Uint8Array): Promise<string> {
	const doc = await open(pdf);
	try {
		let text = '';
		for (let n = 1; n <= doc.numPages; n++) {
			for (const item of (await (await doc.getPage(n)).getTextContent()).items) {
				text += (item.str ?? '') + (item.hasEOL ? '\n' : '');
			}
			text += '\n\f';
		}
		return text;
	} finally {
		await doc.destroy();
	}
}
