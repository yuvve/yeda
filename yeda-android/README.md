# yeda-android

<!-- embed -->
The Android app is for reading and annotating your library on a phone or tablet. Adding references, citation graphs, literature tables, finding PDFs and citing stay in the terminal app. The interface is high-contrast and has no animations, so it suits e-ink screens.

Install the APK from the [releases](https://github.com/yuvve/yeda/releases).

<p>
  <img src="screenshots/list.png" width="200" alt="Reference list">
  <img src="screenshots/detail.png" width="200" alt="A paper">
  <img src="screenshots/annotations.png" width="200" alt="Annotations">
  <img src="screenshots/reader.png" width="200" alt="Reader">
</p>

## Getting started

On first start, pick the library folder with the system folder picker and allow all-files access. To get a library onto the device, sync its folder with Syncthing, or clone its git repository from Settings into an empty folder. When a PDF location's folder isn't found on the device, the app asks for it once.

## Main window

- **All ▾**: pick a collection, tag or saved search.
- **Search**: the same search as the terminal app, such as `author:smith tag:ml`; a search can be saved.
- **Filter**: a form for searching by field. **▲ key** sorts.
- **⟳**: git sync. **⚙**: settings.

Tap a paper to open it. There you open or replace its PDF, preview its annotations, edit its Markdown note or its raw file, and change its reading status, tags and collection.

## Reading and annotating

The reader is the same as on the desktop. Once a pen has been used, the pen draws and a finger pans (two fingers zoom), a resting palm is ignored, and the pen's side button or eraser end erases. Settings → Display → E-ink screen shows one page at a time, turned by swiping, with black ink.

PDFs from outside the library open with "Open with"; their annotations go in a `<file>.annotations/` folder next to the PDF.

## Sync

⟳ commits everything, merges `origin`'s branch of the same name and pushes, over HTTPS. Conflicting annotation files are merged annotation by annotation; any other conflict stops the sync.

## Settings

Settings → WebDAV location adds a WebDAV folder for PDFs after a test login. Settings can also be edited as text: the library folder, and `[locations]` for folders on this device. Passwords and tokens are kept in the Android Keystore.
<!-- /embed -->

## Building

```sh
npm ci && npm run build:reader -w yeda-reader      # at the repository root
cd yeda-android
npx tauri android build --apk --target aarch64     # needs ANDROID_HOME and NDK_HOME
npm run build && cargo run -p yeda-android         # the same app on the desktop
```

`src-tauri/gen/android` is committed because it is customized. Pushing an `android-v*` tag builds a signed APK and publishes it as a release.
