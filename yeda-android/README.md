# yeda-android

<!-- embed -->
The Android app is for reading and annotating your library on a phone or tablet. Adding references, citation graphs, literature tables, finding PDFs and citing stay in the terminal app. The interface is high-contrast and has no animations, so it suits e-ink screens.

Install the APK from the [latest release](https://github.com/yuvve/yeda/releases/latest). Pre-releases are betas.

<p>
  <img src="screenshots/list.png" width="200" alt="Reference list">
  <img src="screenshots/detail.png" width="200" alt="A paper">
  <img src="screenshots/annotations.png" width="200" alt="Annotations">
  <img src="screenshots/reader.png" width="200" alt="Reader">
</p>

## Getting started

On first start, pick the library folder and allow all-files access. Get a library onto the device by syncing its folder with Syncthing, or by cloning its git repository from the settings. When a PDF location's folder isn't found on the device, the app asks for it once.

## Features

- **Browse:** filter by collection, tag or saved search; search with the terminal app's syntax; sort.
- **Paper page:** open or add PDFs, preview annotations, edit the Markdown note, change status, tags and collection.
- **Reader:** the same as on the desktop, with Zotero-compatible annotations, column mode and a pen mode (the pen draws, a finger pans, a palm is ignored). An e-ink mode (Settings → Display → E-ink screen) shows one page at a time, turned by swiping, with black ink.
- **Open with:** PDFs from outside the library open with their annotations in a `<file>.annotations/` folder.
- **Sync:** commits, merges `origin` and pushes over HTTPS. Conflicting annotation files are merged annotation by annotation; any other conflict stops the sync.
- **PDFs:** from local folders, WebDAV or Dropbox. Passwords and tokens are kept in the Android Keystore. Settings has the form that adds a storage location.

The app icon is from Flaticon: [Knowledge graph icons created by Jagat Icon - Flaticon](https://www.flaticon.com/free-icons/knowledge-graph).
<!-- /embed -->

## Building

```sh
npm ci && npm run build:reader -w yeda-reader      # at the repository root
cd yeda-android
npx tauri android build --apk --target aarch64     # needs ANDROID_HOME and NDK_HOME
npm run build && cargo run -p yeda-android         # the same app on the desktop
```

`src-tauri/gen/android` is committed because it is customized. Pushing an `android-v*` tag builds a signed APK and publishes it as a pre-release; a `v*` release includes it too.
