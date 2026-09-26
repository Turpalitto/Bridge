package app.dropbridge.app

import android.content.Intent
import android.graphics.drawable.Icon
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import androidx.annotation.RequiresApi

/**
 * 2026 Android Quick Settings Tile for DropBridge.
 *
 * Allows smartphone users to toggle DropBridge discoverability / receive mode
 * directly from the system notification shade with a single tap.
 */
@RequiresApi(Build.VERSION_CODES.N)
class DropBridgeTileService : TileService() {

    override fun onStartListening() {
        super.onStartListening()
        updateTileState(isActive = isDropBridgeActive())
    }

    override fun onClick() {
        super.onClick()
        val currentlyActive = isDropBridgeActive()
        val newState = !currentlyActive
        saveDropBridgeState(newState)
        updateTileState(isActive = newState)

        if (newState) {
            // Launch MainActivity to start receiver if not already active
            val intent = Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            startActivityAndCollapse(intent)
        }
    }

    private fun updateTileState(isActive: Boolean) {
        val tile = qsTile ?: return
        tile.state = if (isActive) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = "DropBridge"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            tile.subtitle = if (isActive) "Готов к приёму" else "Отключено"
        }
        tile.icon = Icon.createWithResource(this, android.R.drawable.stat_sys_upload)
        tile.updateTile()
    }

    private fun isDropBridgeActive(): Boolean {
        val prefs = getSharedPreferences(PREFS_NAME, MODE_PRIVATE)
        return prefs.getBoolean(KEY_ACTIVE, true)
    }

    private fun saveDropBridgeState(active: Boolean) {
        val prefs = getSharedPreferences(PREFS_NAME, MODE_PRIVATE)
        prefs.edit().putBoolean(KEY_ACTIVE, active).apply()
    }

    companion object {
        private const val PREFS_NAME = "dropbridge_prefs"
        private const val KEY_ACTIVE = "quick_tile_active"
    }
}
