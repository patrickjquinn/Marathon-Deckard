package org.marathonos.deckard.system;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.net.Uri;
import android.os.IBinder;
import android.util.Log;

import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;

/**
 * Lays apps out on the whole display. Marathon gives Android a display about
 * as wide as it is tall, where an app locked to portrait was letterboxed to a
 * strip. Android 16 lifts orientation locks on large screens only, and apps
 * may opt out of that (TikTok does); this per-app override applies on any
 * display and wins over the opt-out. Games keep their orientation.
 */
final class FillDisplay {
    private static final String TAG = "DeckardFillDisplay";
    private static final int DISPLAY = 0;
    private static final int USER = 0;
    private static final long OVERRIDE_ANY_ORIENTATION_TO_USER = 310816437L;

    private FillDisplay() {}

    static void start(Context c) {
        try {
            Object wm = service("window", "android.view.IWindowManager$Stub");
            wm.getClass().getMethod("setIgnoreOrientationRequest", int.class, boolean.class)
                    .invoke(wm, DISPLAY, true);
        } catch (Throwable t) {
            Log.w(TAG, "display keeps orientation requests", t);
        }
        IntentFilter added = new IntentFilter(Intent.ACTION_PACKAGE_ADDED);
        added.addDataScheme("package");
        c.registerReceiver(new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                Uri data = intent.getData();
                if (data != null) fill(context, data.getSchemeSpecificPart());
            }
        }, added, Context.RECEIVER_EXPORTED);
        Intent launcher = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER);
        Set<String> apps = new HashSet<>();
        for (ResolveInfo r : c.getPackageManager().queryIntentActivities(launcher, 0))
            apps.add(r.activityInfo.packageName);
        for (String pkg : apps) fill(c, pkg);
    }

    private static void fill(Context c, String pkg) {
        try {
            ApplicationInfo app = c.getPackageManager().getApplicationInfo(pkg, 0);
            if (app.category == ApplicationInfo.CATEGORY_GAME) return;
            Object compat = service("platform_compat", "com.android.internal.compat.IPlatformCompat$Stub");
            boolean on = (boolean) compat.getClass()
                    .getMethod("isChangeEnabledByPackageName", long.class, String.class, int.class)
                    .invoke(compat, OVERRIDE_ANY_ORIENTATION_TO_USER, pkg, USER);
            if (on) return;
            Class<?> builder = Class.forName("android.app.compat.PackageOverride$Builder");
            Object b = builder.getConstructor().newInstance();
            builder.getMethod("setEnabled", boolean.class).invoke(b, true);
            Map<Long, Object> overrides = new HashMap<>();
            overrides.put(OVERRIDE_ANY_ORIENTATION_TO_USER, builder.getMethod("build").invoke(b));
            Class<?> config = Class.forName("com.android.internal.compat.CompatibilityOverrideConfig");
            Object o = config.getConstructor(Map.class).newInstance(overrides);
            compat.getClass().getMethod("putOverridesOnReleaseBuilds", config, String.class)
                    .invoke(compat, o, pkg);
            Log.i(TAG, pkg + " fills the display");
        } catch (PackageManager.NameNotFoundException e) {
            // Removed again before the broadcast arrived.
        } catch (Throwable t) {
            Log.w(TAG, pkg + " keeps its orientation", t);
        }
    }

    private static Object service(String name, String stub) throws Exception {
        IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, name);
        return Class.forName(stub).getMethod("asInterface", IBinder.class).invoke(null, binder);
    }
}
