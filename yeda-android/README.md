# yeda-android

An Android app (Tauri v2) with a library browser and yeda-reader built in for PDFs. The interface is high-contrast and has no animations, for e-ink screens.

The browser covers what yeda-tui does:
- collections, tags in their colours, saved searches, field and fuzzy search, a filter form and sorting;
- reading status, tags, collections and tag colours;
- adding a reference by DOI, arXiv ID or ISBN, and deleting one;
- previewing annotations with their images;
- attaching a PDF, to a local or a WebDAV location, and opening PDFs from WebDAV;
- editing the Markdown note and the raw reference file;
- extracting a PDF's embedded annotations;
- filling literature tables with an LLM, and exporting them to CSV in the Download folder.

The app reads the library through all-files access, and the library folder is chosen with the system folder picker. A PDF location whose folder isn't found on the device is asked for once and remembered. The app also registers for "Open with" on PDFs; annotations then go in a sidecar `<file>.annotations/` folder.

Settings live in `settings.toml` in the app's data folder. It holds the library folder, plus anything the desktop's `~/.config/yeda/config.toml` takes, such as per-device `[locations]`. It acts as the library's machine config and can be edited as text in the app. WebDAV passwords and LLM API keys go in the Android Keystore, never in this file.

Where yeda-tui uses poppler, the app uses the pdf.js that ships with the reader. It supplies the highlight text and image PNGs for extracted annotations, and the paper text for LLM providers that take text.

## Building

```sh
npm ci && npm run build:reader -w yeda-reader      # at the repository root
cd yeda-android
npx tauri android build --apk --target aarch64     # needs ANDROID_HOME and NDK_HOME
npm run build && cargo run -p yeda-android         # the same app on the desktop
```

`src-tauri/gen/android` is committed because `MainActivity.kt` (the folder picker, file access and "Open with" bridge) and `AndroidManifest.xml` are customized. CI builds a signed APK on every push to `main`.
