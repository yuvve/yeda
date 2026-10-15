# yeda-android

An Android app (Tauri v2) with a library browser and yeda-reader built in for PDFs. The interface is high-contrast and has no animations, for e-ink screens.

The browser covers what yeda-tui does:
- collections, tags in their colours, saved searches, and field and fuzzy search;
- reading status, tags, collections and tag colours;
- adding a reference by DOI, arXiv ID or ISBN, and deleting one;
- previewing annotations with their images;
- editing the Markdown note and the raw reference file;
- extracting a PDF's embedded annotations;
- filling literature tables with an LLM, and exporting them to CSV in the Download folder.

The app reads the library through all-files access, and the library folder is chosen with the system folder picker. A PDF location whose folder isn't found on the device is asked for once and remembered. The app also registers for "Open with" on PDFs; annotations then go in a sidecar `<file>.annotations/` folder.

Settings live in the app's data folder (`settings.toml`): the library folder, per-device `[locations]` and LLM API keys. They act as the library's machine config.

Without poppler on the device, some features do less than on the desktop:
- Extracted highlights carry no text, and extracted image annotations get no PNG.
- LLM tables need an Anthropic profile, which reads the PDF itself.

## Building

```sh
npm ci && npm run build:reader -w yeda-reader      # at the repository root
cd yeda-android
npx tauri android build --apk --target aarch64     # needs ANDROID_HOME and NDK_HOME
npm run build && cargo run -p yeda-android         # the same app on the desktop
```

`src-tauri/gen/android` is committed because `MainActivity.kt` (the folder picker, file access and "Open with" bridge) and `AndroidManifest.xml` are customized. CI builds a signed APK on every push to `main`.
