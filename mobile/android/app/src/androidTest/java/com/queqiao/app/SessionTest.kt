package com.queqiao.app

import android.graphics.Bitmap
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SemanticsNodeInteractionsProvider
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.click
import androidx.compose.ui.test.swipeUp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * The app as a person uses it, against a real server and a PC (scripts/run_android_e2e.sh starts both and passes
 * them as instrumentation arguments: server, pin, user, password, device): log in, the PC in the device list,
 * connect, the picture decoded and shown, touches and typing sent, disconnect. Pictures of each screen go to the
 * app's files (pulled by the script).
 */
@OptIn(ExperimentalTestApi::class)
@RunWith(AndroidJUnit4::class)
class SessionTest {
    @get:Rule
    val rule = createAndroidComposeRule<MainActivity>()

    private val args = InstrumentationRegistry.getArguments()

    private fun arg(name: String): String = args.getString(name) ?: error("missing instrumentation argument $name")

    private fun SemanticsNodeInteractionsProvider.exists(tag: String): Boolean =
        onAllNodes(hasTestTag(tag)).fetchSemanticsNodes().isNotEmpty()

    private fun picture(name: String) {
        Thread.sleep(500)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val bitmap = instrumentation.uiAutomation.takeScreenshot() ?: return
        val folder = File(instrumentation.targetContext.filesDir, "test_pictures").apply { mkdirs() }
        File(folder, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    @Test
    fun logInConnectControlDisconnect() {
        val device = arg("device")
        // A saved login from an earlier run: log out first.
        rule.waitUntil(20_000) { rule.exists("user") || rule.exists("menu") }
        if (rule.exists("menu")) {
            rule.onNodeWithTag("menu").performClick()
            rule.onNodeWithText(rule.activity.getString(R.string.log_out)).performClick()
            rule.waitUntil(20_000) { rule.exists("user") }
        }
        picture("login")
        rule.onNodeWithTag("user").performTextInput(arg("user"))
        rule.onNodeWithTag("password").performTextInput(arg("password"))
        if (!rule.exists("server")) rule.onNodeWithText(rule.activity.getString(R.string.other_server)).performClick()
        rule.onNodeWithTag("server").performTextInput(arg("server"))
        rule.onNodeWithTag("pin").performTextInput(arg("pin"))
        rule.onNodeWithTag("login").performClick()
        rule.waitUntil(40_000) { rule.exists("connect:$device") || rule.exists("error") }
        assertTrue("logged in and the PC listed", rule.exists("connect:$device"))
        picture("devices")

        val before = VideoDecoder.shownTotal
        rule.onNodeWithTag("connect:$device").performClick()
        rule.waitUntil(60_000) { VideoDecoder.shownTotal - before >= 30 }
        picture("session")

        // Touch mode: a tap clicks where it lands, two fingers scroll; then typing through the keyboard path.
        rule.onNodeWithTag("picture").performTouchInput { click(center) }
        rule.onNodeWithTag("picture").performTouchInput { swipeUp() }
        rule.activity.runOnUiThread {
            val view = findSession()
            view?.type("queqiao 鹊桥\n")
        }
        Thread.sleep(2000)
        rule.onNodeWithTag("ball").performClick()
        picture("toolbar")
        rule.onNodeWithTag("disconnect").performClick()
        rule.waitUntil(20_000) { rule.exists("connect:$device") }
        picture("after")
    }

    private fun findSession(): SessionView? {
        fun search(view: android.view.View): SessionView? {
            if (view is SessionView) return view
            if (view is android.view.ViewGroup) {
                for (i in 0 until view.childCount) search(view.getChildAt(i))?.let { return it }
            }
            return null
        }
        return search(rule.activity.window.decorView)
    }
}
