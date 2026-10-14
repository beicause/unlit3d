package org.unlit3d.example

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.os.Process
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import com.google.android.material.tabs.TabLayout

/** One example the app can run: the activity that starts it and its tab's label. */
data class ExampleTab(val label: String, val activity: Class<out Activity>)

/** The examples the app offers, in the order the tab bar lists them. */
val EXAMPLES = listOf(
    ExampleTab("unlit3d", MainActivity::class.java),
    ExampleTab("unlit3dx", Unlit3dxActivity::class.java),
)

/**
 * The shared half of the example activities: a tab bar floating over the
 * bottom of the game, which switches to the other example when its tab is
 * picked.
 *
 * `GameActivity` loads the shared library named by the activity's
 * `android.app.lib_name` manifest entry, so one activity runs exactly one
 * example. The bar is therefore a switcher between activities rather than a
 * container inside one: picking another tab starts its activity and ends this
 * one, which is what keeps a single native window alive at a time.
 *
 * Each activity is declared with its own `android:process`: `GameActivity`
 * keeps the one native handle it can own in a process-wide static, so two
 * activities sharing a process cannot both be alive.
 */
abstract class TabbedExampleActivity : GameActivity() {
    /** The examples the bar offers, in order. */
    protected abstract val tabs: List<ExampleTab>

    /** Which entry of [tabs] this activity runs. */
    protected abstract val selectedTab: Int

    private var bar: TabLayout? = null

    override fun onPostResume() {
        super.onPostResume()
        // The native side owns the content view and sets it up as the window
        // is created, so the bar is added once that has happened and again
        // whenever it is gone — after the window was recreated, say.
        if (bar?.parent == null) installTabs()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)

        if (hasFocus) {
            hideSystemUi()
        }
    }

    private fun installTabs() {
        val density = resources.displayMetrics.density
        val bar = TabLayout(this)
        for (tab in tabs) {
            bar.addTab(bar.newTab().setText(tab.label))
        }
        bar.tabMode = TabLayout.MODE_FIXED
        // Set here rather than left to the theme: the activity's theme is the
        // app's, which is not a Material one, so the attributes a TabLayout
        // would read from it are unset.
        bar.setSelectedTabIndicatorColor(Color.WHITE)
        bar.setTabTextColors(0x99FFFFFF.toInt(), Color.WHITE)
        bar.setBackground(
            GradientDrawable().apply {
                cornerRadius = 20f * density
                setColor(0x66000000)
            },
        )
        bar.getTabAt(selectedTab)?.select()
        bar.addOnTabSelectedListener(
            object : TabLayout.OnTabSelectedListener {
                override fun onTabSelected(tab: TabLayout.Tab) {
                    switchTo(tabs[tab.position])
                }

                override fun onTabUnselected(tab: TabLayout.Tab) = Unit

                override fun onTabReselected(tab: TabLayout.Tab) = Unit
            },
        )

        // The margin keeps the bar clear of the gesture area a phone's home
        // indicator occupies, which the system-UI hiding leaves on screen.
        val margin = (12f * density).toInt()
        addContentView(
            bar,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
                Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL,
            ).apply { bottomMargin = margin },
        )
        this.bar = bar
    }

    private fun switchTo(tab: ExampleTab) {
        if (tab.activity.isInstance(this)) {
            return
        }
        // Started before this activity finishes, so the switch has no gap to
        // flash the launcher in. The other example runs in its own process, so
        // its activity's `onCreate` never terminates this one's native context.
        startActivity(Intent(this, tab.activity))
        finish()
        // Winit does not forward the activity's destroy to the event loop (its
        // Android backend logs a TODO and keeps polling the looper), so the
        // native loop of a finished activity never stops on its own. Ending the
        // process is what releases it; the activity just started lives in
        // another process and is unaffected.
        Process.killProcess(Process.myPid())
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
