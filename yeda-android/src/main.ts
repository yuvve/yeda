import { exit, onBackButtonPress } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { openReader } from '../../yeda-reader/src/reader';
import { takeOpenWithPath } from './bridge';
import { showBrowser } from './browser';

// The browser and the reader are separate hash entries (`#reader`); each reloads the page so the
// reader always starts clean.
window.addEventListener('hashchange', () => location.reload());
const showReader = () => {
	location.hash = 'reader';
};
// Chrome skips history entries added without a user gesture, and the reader's is added after
// an async open, so the default Back handler would find no history and close the app.
let browserBack = () => false;
onBackButtonPress(() => {
	if (location.hash === '#reader') location.hash = '';
	else if (!browserBack()) void exit(0);
}).catch(() => {});

if (location.hash === '#reader') {
	// Android may restart the app on the reader entry with no document open.
	if (!(await openReader())) location.hash = '';
} else {
	const openWith = takeOpenWithPath();
	if (openWith) {
		await invoke('open_pdf', { path: openWith });
		showReader();
	} else {
		browserBack = showBrowser(showReader);
	}
}
