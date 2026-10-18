package org.marathonos.deckard.home;

import android.app.Activity;

/** Android's home task. It draws nothing and is never shown in Marathon. */
public final class HomeActivity extends Activity {
    @Override
    public void onBackPressed() {
    }
}
