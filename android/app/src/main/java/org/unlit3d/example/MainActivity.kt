package org.unlit3d.example

import android.view.WindowManager
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity

/**
 * The activity the example runs in.
 *
 * `GameActivity` loads the shared library named by the `android.app.lib_name`
 * manifest entry and calls its `android_main` on a thread of its own, so there
 * is nothing to do here but take the screen over: the whole application is in
 * Rust.
 */
class MainActivity : GameActivity() {
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)

        if (hasFocus) {
            hideSystemUi()
        }
    }

    private fun hideSystemUi() {
        WindowCompat.enableEdgeToEdge(window)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        val insets = WindowCompat.getInsetsController(window, window.decorView)

        insets.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        insets.hide(
            WindowInsetsCompat.Type.statusBars()
                    or WindowInsetsCompat.Type.navigationBars()
        )
    }
}
