# yeda-android

An Android app (Tauri v2) with a library browser and yeda-reader built in for PDFs. The interface is high-contrast and has no animations, for e-ink screens.

It is for reading and annotating; adding references, citation graphs, literature tables, finding PDFs and citing stay on the desktop. The browser has:
- collections, tags in their colours, saved searches, field and fuzzy search, a filter form and sorting;
- reading status, tags, collections and tag colours, and deleting a reference;
- previewing annotations with their images, and extracting a PDF's embedded annotations;
- attaching a PDF, to a local or a WebDAV location, and opening PDFs from WebDAV;
- editing the Markdown note and the raw reference file;
- git sync over HTTPS (⟳): commit everything, merge `origin`'s branch of the same name and push. Conflicting `annotations.toml` files are merged by annotation `id`; any other conflict stops the sync. Settings can also clone a library repository into an empty folder.

In the reader, once a pen has been used, the pen draws and a finger pans (two fingers zoom) under the drawing tools, a resting palm is ignored, and the pen's side button or eraser end erases. Settings → Display → E-ink screen shows one page at a time, fitted, turned by swiping, with black ink and no animation.

The app reads the library through all-files access, and the library folder is chosen with the system folder picker. A PDF location whose folder isn't found on the device is asked for once and remembered. The app also registers for "Open with" on PDFs; annotations then go in a sidecar `<file>.annotations/` folder.

Settings live in `settings.toml` in the app's data folder. It is the device config: the library folder, and `[locations]` for folders elsewhere on this device. It can be edited as text in the app. WebDAV and git passwords or tokens go in the Android Keystore, never in this file. Settings → WebDAV location adds a location after a test login: `[locations.<name>]` goes to `yeda.toml`, the password to the Keystore.

Where yeda-tui uses poppler, the app uses the pdf.js that ships with the reader. It supplies the highlight text and image PNGs for extracted annotations.

## Building

```sh
npm ci && npm run build:reader -w yeda-reader      # at the repository root
cd yeda-android
npx tauri android build --apk --target aarch64     # needs ANDROID_HOME and NDK_HOME
npm run build && cargo run -p yeda-android         # the same app on the desktop
```

`src-tauri/gen/android` is committed because `MainActivity.kt` (the folder picker, file access and "Open with" bridge) and `AndroidManifest.xml` are customized. Pushing an `android-v*` tag builds a signed APK and publishes it as a release.
