/** Implemented by `MainActivity.Bridge`; async results arrive by calling `window[callback]`. */
interface AndroidBridge {
	hasFileAccess(): boolean;
	requestFileAccess(callback: string): void;
	pickFolder(callback: string): void;
	takeOpenWithPath(): string | null;
}

declare global {
	interface Window {
		YedaAndroid?: AndroidBridge;
		[callback: `yedaCallback${number}`]: ((value: unknown) => void) | undefined;
	}
}

// Outside Android (`npm run tauri dev`) a typed path stands in for the folder picker.
const desktop: AndroidBridge = {
	hasFileAccess: () => true,
	requestFileAccess: callback => window[callback as `yedaCallback${number}`]!(true),
	pickFolder: callback => window[callback as `yedaCallback${number}`]!(window.prompt('Folder path')),
	takeOpenWithPath: () => null,
};

const android = window.YedaAndroid ?? desktop;
let callbacks = 0;

function bridgeCall<T>(call: (callback: string) => void): Promise<T> {
	const name = `yedaCallback${callbacks++}` as const;
	return new Promise((resolve) => {
		window[name] = (value) => {
			delete window[name];
			resolve(value as T);
		};
		call(name);
	});
}

export const hasFileAccess = () => android.hasFileAccess();
export const requestFileAccess = () => bridgeCall<boolean>(cb => android.requestFileAccess(cb));
/** The chosen folder as a file path; null if cancelled or not on local storage. */
export const pickFolder = () => bridgeCall<string | null>(cb => android.pickFolder(cb));
/** The PDF passed through "Open with", consumed by the call. */
export const takeOpenWithPath = () => android.takeOpenWithPath();
