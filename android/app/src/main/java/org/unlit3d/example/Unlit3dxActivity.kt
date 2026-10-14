package org.unlit3d.example

/**
 * The activity that runs the multi-world example.
 *
 * A second activity rather than a mode of [MainActivity]: the library an
 * activity loads is a manifest entry, and `GameActivity` reads it once per
 * activity, so each example needs an activity of its own.
 */
class Unlit3dxActivity : TabbedExampleActivity() {
    override val tabs = EXAMPLES

    override val selectedTab = 1
}
