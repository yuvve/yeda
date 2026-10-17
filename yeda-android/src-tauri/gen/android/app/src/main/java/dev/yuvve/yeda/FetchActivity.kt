package dev.yuvve.yeda

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.webkit.CookieManager
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL

/**
 * Fetches PDFs through the library proxy in a WebView: you sign in and click through where needed.
 * Takes the plan from `fetch_plan` as the "job" extra; returns one result per paper as JSON.
 */
class FetchActivity : Activity() {
  private class Paper(val key: String, val doi: String, val file: String)

  private lateinit var web: WebView
  private lateinit var status: TextView
  private lateinit var papers: List<Paper>
  private lateinit var downloads: File
  private var proxy: String? = null
  private var proxyHost: String? = null
  private var index = -1
  private val tried = mutableSetOf<String>()
  private val results = JSONArray()

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val job = JSONObject(intent.getStringExtra("job")!!)
    downloads = File(job.getString("downloads"))
    proxy = job.optString("proxy").takeIf { it.isNotEmpty() && it != "null" }
    proxyHost = job.optString("proxy_host").takeIf { it.isNotEmpty() && it != "null" }
    val list = job.getJSONArray("papers")
    papers = (0 until list.length()).map { list.getJSONObject(it) }.filter { !it.isNull("doi") }
      .map { Paper(it.getString("key"), it.getString("doi"), it.getString("file")) }

    status = TextView(this).apply { setPadding(16, 8, 16, 8) }
    val skip = Button(this).apply { text = "Skip"; setOnClickListener { fail("skipped in the browser") } }
    val stop = Button(this).apply { text = "Stop"; setOnClickListener { done() } }
    val bar = LinearLayout(this).apply {
      addView(status, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
      addView(skip)
      addView(stop)
    }
    web = WebView(this)
    web.settings.javaScriptEnabled = true
    web.settings.domStorageEnabled = true
    CookieManager.getInstance().setAcceptThirdPartyCookies(web, true)
    web.webViewClient = object : WebViewClient() {
      override fun onPageFinished(view: WebView, url: String) {
        val paper = index
        view.evaluateJavascript(reportScript()) { value -> if (paper == index) onReport(value) }
      }
    }
    // WebView shows no PDFs, so a PDF link always arrives here.
    web.setDownloadListener { url, userAgent, _, _, _ -> fetchPdf(url, userAgent, showIfNot = false) }
    val root = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      addView(bar, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
      addView(web, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
    }
    ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.ime())
      v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
    setContentView(root)
    next()
  }

  @Deprecated("Back stops fetching, like Stop.")
  override fun onBackPressed() = done()

  private fun proxied(url: String) = proxy?.replace("{url}", url) ?: url

  private fun next() {
    index++
    tried.clear()
    if (index >= papers.size) return done()
    val p = papers[index]
    status.text = "${index + 1}/${papers.size} ${p.key}: sign in or tap the PDF link if it stops"
    web.loadUrl(proxied("https://doi.org/${p.doi}"))
  }

  /** The landing page's `citation_pdf_url`, through the proxy when the page came through it. */
  private fun reportScript(): String {
    val proxyJs = JSONObject.quote(proxy ?: "")
    val hostJs = JSONObject.quote(proxyHost ?: "")
    return """(() => {
      const m = document.querySelector('meta[name="citation_pdf_url"]');
      let url = m && m.content ? new URL(m.content, location.href).href : '';
      const proxy = $proxyJs, host = $hostJs;
      if (url && host && location.hostname.endsWith('.' + host) && !new URL(url).hostname.endsWith('.' + host)) url = proxy.replace('{url}', url);
      return url;
    })()"""
  }

  private fun onReport(value: String?) {
    val pdf = value?.let { JSONArray("[$it]").optString(0) }?.takeIf { it.isNotEmpty() && it != "null" } ?: return
    if (!tried.add(pdf)) return
    fetchPdf(pdf, web.settings.userAgentString, showIfNot = true)
  }

  /** Downloads `url` with the WebView's cookies; if it isn't a PDF and `showIfNot`, opens it to show what is in the way. */
  private fun fetchPdf(url: String, userAgent: String, showIfNot: Boolean) {
    val paper = index
    Thread {
      val bytes = runCatching { get(url, userAgent) }.getOrNull()
      runOnUiThread {
        if (paper != index) return@runOnUiThread
        when {
          bytes != null -> {
            val file = File(downloads, papers[index].file)
            file.writeBytes(bytes)
            results.put(JSONObject().put("key", papers[index].key).put("file", file.path))
            next()
          }
          showIfNot -> web.loadUrl(url)
        }
      }
    }.start()
  }

  /** Follows redirects by hand so each hop gets its own host's cookies, as EZproxy needs. */
  private fun get(start: String, userAgent: String): ByteArray? {
    val cookies = CookieManager.getInstance()
    var url = start
    repeat(8) {
      val c = URL(url).openConnection() as HttpURLConnection
      c.instanceFollowRedirects = false
      c.setRequestProperty("User-Agent", userAgent)
      cookies.getCookie(url)?.let { c.setRequestProperty("Cookie", it) }
      c.headerFields.filterKeys { it.equals("Set-Cookie", ignoreCase = true) }.values.flatten().forEach { cookies.setCookie(url, it) }
      if (c.responseCode in 300..399) {
        url = URL(URL(url), c.getHeaderField("Location")).toString()
        return@repeat
      }
      val bytes = c.inputStream.use { it.readBytes() }
      return bytes.takeIf { String(it, 0, minOf(it.size, 1024), Charsets.ISO_8859_1).trimStart().startsWith("%PDF-") }
    }
    return null
  }

  private fun fail(error: String) {
    if (index in papers.indices) results.put(JSONObject().put("key", papers[index].key).put("error", error))
    next()
  }

  private fun done() {
    // The proxy session survives a restart only once written out.
    CookieManager.getInstance().flush()
    setResult(RESULT_OK, Intent().putExtra("results", results.toString()))
    finish()
  }
}
