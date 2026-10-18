package org.marathonos.deckard.system;

import android.app.Application;

/** Started at boot (persistent): keeps app tasks fitted and the host channel open. */
public final class SystemApp extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        HostLink.get();
        WindowFit.start();
    }
}
