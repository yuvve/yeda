import { invoke } from '@tauri-apps/api/core';

export interface Position {
	pageIndex: number;
	rects?: number[][];
	paths?: number[][];
	width?: number;
}

export interface Annotation {
	id: string;
	type: string;
	sortIndex: string;
	position: Position;
	pageLabel?: string;
	color?: string;
	image?: string;
	[key: string]: unknown;
}

type Entry = Record<string, unknown>;

export interface PdfPage {
	getViewport(params: { scale: number }): {
		width: number;
		height: number;
		convertToViewportPoint(x: number, y: number): [number, number];
		convertToViewportRectangle(rect: number[]): number[];
	};
	render(params: { canvasContext: CanvasRenderingContext2D; viewport: unknown }): { promise: Promise<void> };
	getTextContent(): Promise<{ items: { str: string; transform: number[]; width: number; height: number }[] }>;
}

declare global {
	interface Window {
		PDFViewerApplication?: { pdfDocument?: { getPage(n: number): Promise<PdfPage> } };
	}
}

const IMAGE_KINDS = new Set(['image', 'ink']);
const IMAGE_SCALE = 150 / 72;

function toEntry(a: Annotation): Entry {
	const { id, type, pageLabel, sortIndex, image, onlyTextOrComment, readOnly, position, ...rest } = a;
	const entry: Entry = {
		id,
		kind: type,
		page: pageLabel || String(position.pageIndex + 1),
		color: rest.color,
		text: rest.text,
		comment: rest.comment,
		image: IMAGE_KINDS.has(type) ? `img/${id}.png` : null,
		source: 'reader',
		sort_index: sortIndex,
		...rest,
		position,
	};
	// Null tells the backend to remove the key, so a cleared comment disappears from the file.
	for (const [key, value] of Object.entries(entry)) {
		if (value === undefined || value === '' || (Array.isArray(value) && !value.length)) {
			entry[key] = null;
		}
	}
	return entry;
}

function fromEntry(e: Entry): Annotation {
	const { kind, page, sort_index, image, ...rest } = e;
	return {
		// The reader fills these in for annotations it creates but expects them on ones passed in.
		text: '',
		comment: '',
		tags: [],
		...rest,
		type: kind,
		pageLabel: page,
		sortIndex: sort_index,
		// yeda regenerates these from the PDF, so edits would be lost.
		readOnly: rest.source === 'pdf',
	} as unknown as Annotation;
}

const imageKey = (a: Annotation) => JSON.stringify([a.position, a.color]);

/** Loads, saves and renders images for one annotations folder through the Rust backend. */
export class AnnotationStore {
	// What each annotation's PNG was rendered from, to skip re-rendering on comment edits.
	private rendered = new Map<string, string>();

	async load(): Promise<Annotation[]> {
		const annotations = (await invoke<Entry[]>('load_annotations')).map(fromEntry);
		await Promise.all(annotations.filter(a => IMAGE_KINDS.has(a.type)).map(async (a) => {
			try {
				a.image = await dataUrl(await invoke<ArrayBuffer>('read_image', { id: a.id }));
				this.rendered.set(a.id, imageKey(a));
			}
			catch {
				// No PNG yet; rendered once the reader saves or `renderMissing` runs.
			}
		}));
		return annotations;
	}

	async save(annotations: Annotation[], showImage: (a: Annotation) => void) {
		await invoke('save_annotations', { annotations: annotations.map(toEntry) });
		for (const a of annotations.filter(a => IMAGE_KINDS.has(a.type))) {
			if (this.rendered.get(a.id) !== imageKey(a)) {
				showImage({ ...a, image: await this.renderImage(a) });
			}
		}
	}

	async renderMissing(annotations: Annotation[], showImage: (a: Annotation) => void) {
		for (const a of annotations.filter(a => IMAGE_KINDS.has(a.type) && !this.rendered.has(a.id))) {
			showImage({ ...a, image: await this.renderImage(a) });
		}
	}

	async delete(ids: string[]) {
		await invoke('delete_annotations', { ids });
		ids.forEach(id => this.rendered.delete(id));
	}

	/** Writes `img/<id>.png` and returns it as a data URL. */
	private async renderImage(a: Annotation): Promise<string> {
		const canvas = await renderRegion(a);
		const png = await new Promise<Blob>((resolve, reject) => {
			canvas.toBlob(b => (b ? resolve(b) : reject(new Error('PNG encoding failed'))), 'image/png');
		});
		await invoke('save_image', new Uint8Array(await png.arrayBuffer()), { headers: { id: a.id } });
		this.rendered.set(a.id, imageKey(a));
		return canvas.toDataURL('image/png');
	}
}

async function renderRegion(a: Annotation): Promise<HTMLCanvasElement> {
	const doc = window.PDFViewerApplication?.pdfDocument;
	if (!doc) throw new Error('PDF not loaded');
	const page = await doc.getPage(a.position.pageIndex + 1);
	const viewport = page.getViewport({ scale: IMAGE_SCALE });

	// pdf.js registers the PDF's fonts in the reader iframe's document; a canvas elsewhere renders tofu.
	const iframe = document.querySelector<HTMLIFrameElement>('#primary-view iframe');
	const full = (iframe?.contentDocument ?? document).createElement('canvas');
	full.width = Math.ceil(viewport.width);
	full.height = Math.ceil(viewport.height);
	const ctx = full.getContext('2d')! as CanvasRenderingContext2D & { skipBlender?: boolean };
	// Zotero's pdf.js recolours canvases for the reader's dark theme; images keep the PDF's own colours.
	ctx.skipBlender = true;
	ctx.fillStyle = '#ffffff';
	ctx.fillRect(0, 0, full.width, full.height);
	await page.render({ canvasContext: ctx, viewport }).promise;

	// Ink lives only in the annotation, not in the PDF, so it is drawn on top of the page.
	const { paths, width = 1 } = a.position;
	if (paths) {
		ctx.lineCap = ctx.lineJoin = 'round';
		ctx.lineWidth = width * IMAGE_SCALE;
		ctx.strokeStyle = a.color ?? '#000000';
		for (const path of paths) {
			ctx.beginPath();
			for (let i = 0; i < path.length; i += 2) {
				ctx.lineTo(...viewport.convertToViewportPoint(path[i], path[i + 1]));
			}
			ctx.stroke();
		}
	}

	const [x1, y1, x2, y2] = viewport.convertToViewportRectangle(pdfRect(a.position));
	const x = Math.max(0, Math.floor(Math.min(x1, x2)));
	const y = Math.max(0, Math.floor(Math.min(y1, y2)));
	const crop = document.createElement('canvas');
	crop.width = Math.max(1, Math.min(full.width, Math.ceil(Math.max(x1, x2))) - x);
	crop.height = Math.max(1, Math.min(full.height, Math.ceil(Math.max(y1, y2))) - y);
	crop.getContext('2d')!.drawImage(full, x, y, crop.width, crop.height, 0, 0, crop.width, crop.height);
	return crop;
}

function pdfRect({ rects, paths, width = 1 }: Position): number[] {
	if (rects?.length) return rects[0];
	const xs = paths!.flatMap(p => p.filter((_, i) => i % 2 === 0));
	const ys = paths!.flatMap(p => p.filter((_, i) => i % 2 === 1));
	return [Math.min(...xs) - width, Math.min(...ys) - width, Math.max(...xs) + width, Math.max(...ys) + width];
}

function dataUrl(buf: ArrayBuffer): Promise<string> {
	return new Promise((resolve, reject) => {
		const reader = new FileReader();
		reader.onload = () => resolve(reader.result as string);
		reader.onerror = () => reject(reader.error);
		reader.readAsDataURL(new Blob([buf], { type: 'image/png' }));
	});
}
