package org.unlit3d.example

/**
 * The activity that runs the single-world example.
 *
 * `GameActivity` loads the shared library named by the `android.app.lib_name`
 * manifest entry and calls its `android_main` on a thread of its own, so there
 * is nothing to do here but take the screen over: the whole application is in
 * Rust. The tab bar comes from [TabbedExampleActivity], which is what lets the
 * other example be started from here.
 */
class MainActivity : TabbedExampleActivity() {
    override val tabs = EXAMPLES

    override val selectedTab = 0
}
