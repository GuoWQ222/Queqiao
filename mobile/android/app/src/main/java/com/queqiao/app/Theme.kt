package com.queqiao.app

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/** Queqiao's colours, as the desktop windows have them (main_window.rs, linux/theme.rs). */
object Q {
    val Night = Color(0xFF141A3B)
    val NightSelected = Color(0xFF263169)
    val NightText = Color(0xFFD2D7EE)
    val NightMuted = Color(0xFF8890B5)
    val NightChip = Color(0xFF2D3874)
    val Gold = Color(0xFFE3B45A)
    val GoldHover = Color(0xFFECC26E)
    val OnGold = Color(0xFF1A1F3F)
    val Text = Color(0xFF1B2140)
    val Secondary = Color(0xFF646B8C)
    val Disabled = Color(0xFFB4B9CC)
    val Divider = Color(0xFFE6E9F2)
    val Card = Color(0xFFF5F6FA)
    val Accent = Color(0xFF3D5FAE)
    /** The being-controlled banner's pale blue (as the desktop main window's). */
    val BannerBack = Color(0xFFEAF2FE)
    val Online = Color(0xFF2FBF71)
    val Offline = Color(0xFF9AA1B8)
    val Border = Color(0xFFCBCFDA)
    val HeroTop = Color(0xFF253070)
    val HeroBottom = Color(0xFF0E1330)
    val HeroMuted = Color(0xFFA9B1D6)
    val PillOnline = Color(0xFFE3F6EC)
    val PillOnlineText = Color(0xFF17784A)
    val PillOffline = Color(0xFFEEF0F5)
    val PillOfflineText = Color(0xFF5D6480)
    val Danger = Color(0xFFC6423B)
}

@Composable
fun QueqiaoTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = lightColorScheme(
            primary = Q.Accent,
            onPrimary = Color.White,
            secondary = Q.Gold,
            onSecondary = Q.OnGold,
            background = Color.White,
            surface = Color.White,
            onSurface = Q.Text,
            onBackground = Q.Text,
            outline = Q.Border,
            error = Q.Danger,
        ),
        content = content,
    )
}
