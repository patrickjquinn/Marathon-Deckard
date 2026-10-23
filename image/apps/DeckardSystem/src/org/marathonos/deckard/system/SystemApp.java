package org.marathonos.deckard.system;

import android.app.Application;

/** Started at boot (persistent): keeps app tasks fitted, the host channel open and Marathon the handler of its schemes. */
public final class SystemApp extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        HostLink.get();
        WindowFit.start();
        ToastBar.start();
        GoogleServices.prepare(this);
        Handlers.start(this);
    }
}
