import { invoke } from '@tauri-apps/api/core';
import { openReader } from '../../yeda-reader/src/reader';
import { takeOpenWithPath } from './bridge';
import { showBrowser } from './browser';

// The browser and the reader are separate history entries (`#reader`), so Back returns to the
// browser; each entry reloads the page so the reader always starts clean.
window.addEventListener('hashchange', () => location.reload());
const showReader = () => {
	location.hash = 'reader';
};

if (location.hash === '#reader') {
	// Android may restart the app on the reader entry with no document open.
	if (!(await openReader())) location.hash = '';
} else {
	const openWith = takeOpenWithPath();
	if (openWith) {
		await invoke('open_pdf', { path: openWith });
		showReader();
	} else {
		showBrowser(showReader);
	}
}
