package dev.yuvve.yeda

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.provider.Settings
import android.webkit.JavascriptInterface
import android.webkit.WebView
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONArray
import java.io.File

class MainActivity : TauriActivity() {
  private lateinit var webView: WebView
  private var openWithUri: Uri? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    openWithUri = viewedUri(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    viewedUri(intent)?.let {
      openWithUri = it
      runOnUiThread { webView.evaluateJavascript("location.replace('index.html')", null) }
    }
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(Bridge(), "YedaAndroid")
    // Edge-to-edge is forced from targetSdk 35; pad instead of handling insets in each page.
    ViewCompat.setOnApplyWindowInsetsListener(findViewById<View>(android.R.id.content)) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.ime())
      v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }

  private fun viewedUri(intent: Intent?): Uri? =
    intent?.takeIf { it.action == Intent.ACTION_VIEW }?.data

  /** Called from the start screen's JavaScript; results of async calls come back through `window.<callback>`. */
  inner class Bridge {
    @JavascriptInterface
    fun hasFileAccess(): Boolean =
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) Environment.isExternalStorageManager()
      else checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED

    @JavascriptInterface
    fun requestFileAccess(callback: String) = runOnUiThread {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        val settings = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:$packageName"))
        launchActivityForResult(settings) { reply(callback, hasFileAccess()) }
      } else {
        val permissions = arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE, Manifest.permission.WRITE_EXTERNAL_STORAGE)
        requestPermissions(permissions) { reply(callback, hasFileAccess()) }
      }
    }

    /** Replies with the chosen folder as a file path, or null if cancelled or not on local storage. */
    @JavascriptInterface
    fun pickFolder(callback: String) = runOnUiThread {
      launchActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)) { result ->
        val path = result?.data?.data?.let { treePath(DocumentsContract.getTreeDocumentId(it)) }
        reply(callback, path)
      }
    }

    /** Replies with a PDF chosen with the system file picker, as a path, or null if cancelled. */
    @JavascriptInterface
    fun pickPdf(callback: String) = runOnUiThread {
      val pick = Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("application/pdf")
      launchActivityForResult(pick) { result ->
        reply(callback, result?.data?.data?.let { filePath(it) ?: copyToAppStorage(it) })
      }
    }

    /** Fetches PDFs in `FetchActivity` with the plan from `fetch_plan`; replies with its results as JSON. */
    @JavascriptInterface
    fun fetchInBrowser(job: String, callback: String) = runOnUiThread {
      launchActivityForResult(Intent(this@MainActivity, FetchActivity::class.java).putExtra("job", job)) { result ->
        reply(callback, result?.data?.getStringExtra("results"))
      }
    }

    /** The PDF passed through "Open with", as a path; consumed by the call. */
    @JavascriptInterface
    fun takeOpenWithPath(): String? {
      val uri = openWithUri ?: return null
      openWithUri = null
      return filePath(uri) ?: copyToAppStorage(uri)
    }
  }

  private fun reply(callback: String, value: Any?) = runOnUiThread {
    webView.evaluateJavascript("window.$callback(${JSONArray().put(value)}[0])", null)
  }

  /** Maps a storage document id such as `primary:Books/lib` or `1A2B-3C4D:lib` to a path. */
  private fun treePath(documentId: String): String? {
    val (volume, rel) = documentId.split(":", limit = 2).takeIf { it.size == 2 } ?: return null
    val root = if (volume == "primary") Environment.getExternalStorageDirectory() else File("/storage/$volume")
    return File(root, rel).path
  }

  private fun filePath(uri: Uri): String? = when {
    uri.scheme == "file" -> uri.path
    uri.authority == "com.android.externalstorage.documents" -> treePath(DocumentsContract.getDocumentId(uri))
    // Most file managers serve their own content:// URIs; the MediaStore path column still resolves many of them.
    else -> runCatching {
      contentResolver.query(uri, arrayOf("_data"), null, null, null)?.use { c ->
        if (c.moveToFirst()) c.getString(0) else null
      }
    }.getOrNull()?.takeIf { File(it).canRead() }
  }

  /** Copies an unresolvable PDF into app storage so its sidecar annotations folder has somewhere to live. */
  private fun copyToAppStorage(uri: Uri): String? = runCatching {
    val name = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
      if (c.moveToFirst()) c.getString(0) else null
    } ?: "document.pdf"
    val target = File(File(filesDir, "open-with").apply { mkdirs() }, File(name).name)
    contentResolver.openInputStream(uri)!!.use { input -> target.outputStream().use { input.copyTo(it) } }
    target.path
  }.getOrNull()
}
