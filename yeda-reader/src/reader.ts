import { invoke } from '@tauri-apps/api/core';
import { Annotation, AnnotationStore } from './annotations';
import { installColumns } from './columns';
import { installPen, PenReader, settleEink } from './pen';
import { installVim, VimReader } from './vim';

interface Reader extends Omit<VimReader, '_state'>, Omit<PenReader, '_state'> {
	_state: VimReader['_state'] & PenReader['_state'];
	initializedPromise: Promise<unknown>;
	setAnnotations(annotations: Annotation[]): void;
}

declare global {
	interface Window {
		createReader(options: object): Reader;
	}
}

interface Launch {
	title: string;
	page: number | null;
}

const noop = () => {};

// The reader calls these unconditionally; the rest of its callbacks are optional.
const requiredCallbacks = Object.fromEntries([
	'onAddToNote', 'onChangeSidebarView', 'onChangeSidebarWidth', 'onChangeViewState',
	'onDeletePages', 'onOpenLink', 'onOpenReadAloudFirstRunPopup',
	'onReadAloudEngineStateChanged', 'onRotatePages', 'onSaveCustomThemes',
	'onSetDarkTheme', 'onSetDataTransferAnnotations', 'onSetLightTheme', 'onSetReadAloudVoice',
	'onSetZoom', 'onTextSelectionAnnotationModeChange', 'onToggleSidebar',
].map(name => [name, noop]));

// WebKitGTK and Android WebView lack speechSynthesis, which the reader's Read Aloud touches on startup.
if (!('speechSynthesis' in window)) {
	Object.defineProperty(window, 'speechSynthesis', {
		value: Object.assign(new EventTarget(), { getVoices: () => [], speak: noop, cancel: noop }),
	});
}

export function reportError(action: string) {
	return (e: unknown) => {
		console.error(e);
		window.alert(`${action} failed: ${e}`);
	};
}

/** Shows the document the backend has open; `false` if none is. */
export async function openReader(): Promise<boolean> {
	let reader: Reader | undefined;
	let toggleColumns = noop;
	const hookVimIntoView = installVim(() => reader, () => toggleColumns());
	const store = new AnnotationStore();
	const launch = await invoke<Launch | null>('launch');
	if (!launch) return false;
	const buf = new Uint8Array(await invoke<ArrayBuffer>('read_pdf'));
	const annotations = await store.load().catch((e) => {
		reportError('Loading annotations')(e);
		return [];
	});
	const showImage = (a: Annotation) => reader!.setAnnotations([a]);

	document.title = launch.title;
	const attachColumns = installColumns(launch.title);
	// zotero/reader lays its toolbar out for a desktop window; on phones it scrolls sideways instead.
	document.head.append(Object.assign(document.createElement('style'), {
		// zotero/reader positions the "/ N" page count absolutely, so it needs real width once items stop shrinking.
		textContent: '@media (max-width: 799px) { .toolbar { overflow-x: auto; scrollbar-width: none; } .toolbar > * { flex-shrink: 0; } .toolbar #numPages div { position: static; } }',
	}));
	reader = window.createReader({
		...requiredCallbacks,
		type: 'pdf',
		data: { buf },
		title: launch.title,
		annotations,
		primaryViewState: launch.page ? { pageIndex: launch.page - 1 } : undefined,
		readOnly: false,
		showAnnotations: true,
		sidebarOpen: window.innerWidth >= 800,
		sidebarWidth: 240,
		sidebarView: 'annotations',
		authorName: '',
		onConfirm: (title: string, text: string) => window.confirm(`${title}\n\n${text}`),
		onSaveAnnotations: (changed: Annotation[]) => store.save(changed, showImage).catch(reportError('Saving annotations')),
		onDeleteAnnotations: (ids: string[]) => store.delete(ids).catch(reportError('Deleting annotations')),
	});

	hookVimIntoView(reader);
	installPen(reader);
	toggleColumns = attachColumns(reader);

	settleEink(reader).catch(reportError('Setting up e-ink mode'));
	reader.initializedPromise
		.then(() => store.renderMissing(annotations, showImage))
		.catch(reportError('Rendering annotation images'));
	return true;
}
