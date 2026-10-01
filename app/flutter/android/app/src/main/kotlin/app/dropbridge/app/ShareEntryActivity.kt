package app.dropbridge.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import java.io.File

/**
 * Invisible share target. Stages SEND / SEND_MULTIPLE content into the app
 * directory (bytes never go through Dart), then forwards to [MainActivity].
 * text/plain becomes a .txt file; URLs become .url stubs.
 */
class ShareEntryActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Thread {
            val staged = ArrayList<String>()
            val dir = File(filesDir, "staged").apply { mkdirs() }
            pruneStagedFiles(dir)
            when (intent?.action) {
                Intent.ACTION_SEND -> {
                    (intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let {
                        stage(it, dir)?.let(staged::add)
                    } ?: run {
                        intent.getStringExtra(Intent.EXTRA_TEXT)?.let { text ->
                            staged.add(stageText(text, dir))
                        }
                    }
                }
                Intent.ACTION_SEND_MULTIPLE -> {
                    intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)
                        ?.forEach { uri -> stage(uri, dir)?.let(staged::add) }
                }
            }
            runOnUiThread {
                val next = Intent(this, MainActivity::class.java).apply {
                    putStringArrayListExtra(MainActivity.EXTRA_STAGED_PATHS, staged)
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                }
                startActivity(next)
                finish()
            }
        }.start()
    }

    private fun stage(uri: Uri, dir: File): String? = runCatching {
        val rawName = queryName(uri) ?: "shared-${System.nanoTime()}"
        val sanitized = File(rawName).name.filter { it.isLetterOrDigit() || it in "._- " }.trim().ifEmpty { "shared-${System.nanoTime()}" }
        val out = File(dir, uniqueName(dir, sanitized))
        if (!out.canonicalPath.startsWith(dir.canonicalPath + File.separator)) {
            return@runCatching null
        }
        contentResolver.openInputStream(uri)?.use { input ->
            out.outputStream().use { input.copyTo(it) }
        } ?: return@runCatching null
        out.absolutePath
    }.getOrNull()

    private fun stageText(text: String, dir: File): String {
        val trimmed = text.trim()
        val isLink = trimmed.startsWith("http://") || trimmed.startsWith("https://")
        val base = if (isLink) "link-${System.currentTimeMillis()}.url" else "note-${System.currentTimeMillis()}.txt"
        val out = File(dir, uniqueName(dir, base))
        if (isLink) {
            out.writeText("[InternetShortcut]\r\nURL=$trimmed\r\n")
        } else {
            out.writeText(text)
        }
        return out.absolutePath
    }

    private fun uniqueName(dir: File, name: String): String {
        var candidate = name
        var i = 1
        while (File(dir, candidate).exists()) {
            val dot = name.lastIndexOf('.')
            candidate = if (dot > 0) "${name.substring(0, dot)}-$i${name.substring(dot)}" else "$name-$i"
            i++
        }
        return candidate
    }

    private fun pruneStagedFiles(dir: File) {
        runCatching {
            val cutoff = System.currentTimeMillis() - 24 * 60 * 60 * 1000L
            dir.listFiles()?.forEach { file ->
                if (file.isFile && file.lastModified() < cutoff) {
                    file.delete()
                }
            }
        }
    }

    private fun queryName(uri: Uri): String? = runCatching {
        contentResolver.query(uri, null, null, null, null)?.use { c ->
            val idx = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
            if (idx >= 0 && c.moveToFirst()) c.getString(idx) else null
        }
    }.getOrNull()
}
