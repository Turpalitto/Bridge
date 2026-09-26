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
        val staged = ArrayList<String>()
        when (intent?.action) {
            Intent.ACTION_SEND -> {
                (intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let {
                    stage(it)?.let(staged::add)
                } ?: run {
                    intent.getStringExtra(Intent.EXTRA_TEXT)?.let { text ->
                        staged.add(stageText(text))
                    }
                }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)
                    ?.forEach { uri -> stage(uri)?.let(staged::add) }
            }
        }
        val next = Intent(this, MainActivity::class.java).apply {
            putStringArrayListExtra(MainActivity.EXTRA_STAGED_PATHS, staged)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        startActivity(next)
        finish()
    }

    private fun stage(uri: Uri): String? = runCatching {
        val name = queryName(uri) ?: "shared-${System.nanoTime()}"
        val dir = File(filesDir, "staged").apply { mkdirs() }
        val out = File(dir, name)
        contentResolver.openInputStream(uri)?.use { input ->
            out.outputStream().use { input.copyTo(it) }
        } ?: return@runCatching null
        out.absolutePath
    }.getOrNull()

    private fun stageText(text: String): String {
        val dir = File(filesDir, "staged").apply { mkdirs() }
        val out = File(dir, if (text.startsWith("http")) "link.url" else "note.txt")
        out.writeText(text)
        return out.absolutePath
    }

    private fun queryName(uri: Uri): String? = runCatching {
        contentResolver.query(uri, null, null, null, null)?.use { c ->
            val idx = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
            if (idx >= 0 && c.moveToFirst()) c.getString(idx) else null
        }
    }.getOrNull()
}
