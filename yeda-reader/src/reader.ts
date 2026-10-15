import { invoke } from '@tauri-apps/api/core';
import { Annotation, AnnotationStore } from './annotations';
import { installVim, VimReader } from './vim';

interface Reader extends VimReader {
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
	const hookVimIntoView = installVim(() => reader);
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
	reader = window.createReader({
		...requiredCallbacks,
		type: 'pdf',
		data: { buf },
		title: launch.title,
		annotations,
		primaryViewState: launch.page ? { pageIndex: launch.page - 1 } : undefined,
		readOnly: false,
		showAnnotations: true,
		sidebarWidth: 240,
		sidebarView: 'annotations',
		authorName: '',
		onConfirm: (title: string, text: string) => window.confirm(`${title}\n\n${text}`),
		onSaveAnnotations: (changed: Annotation[]) => store.save(changed, showImage).catch(reportError('Saving annotations')),
		onDeleteAnnotations: (ids: string[]) => store.delete(ids).catch(reportError('Deleting annotations')),
	});

	hookVimIntoView(reader);

	reader.initializedPromise
		.then(() => store.renderMissing(annotations, showImage))
		.catch(reportError('Rendering annotation images'));
	return true;
}
