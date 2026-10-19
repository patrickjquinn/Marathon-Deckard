package org.marathonos.deckard.system;

import android.content.Context;
import android.content.pm.LauncherApps;
import android.content.pm.ShortcutInfo;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.UserHandle;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * For deckardd's Keel Actions (protocol/bridge.md, Keel Actions): apps'
 * launcher shortcuts, which only a shortcut host such as the system uid may
 * read and start, and whether an app's window is secure.
 */
final class AppActions {
    private static final String TAG = "DeckardActions";
    private static final ExecutorService WORKER = Executors.newSingleThreadExecutor();
    private static final int MAX_SHORTCUTS = 15;
    private static boolean sWatching;

    private AppActions() {}

    static void handle(JSONObject m) {
        WORKER.execute(() -> {
            JSONObject r;
            try {
                Context c = (Context) Class.forName("android.app.ActivityThread")
                        .getMethod("currentApplication").invoke(null);
                r = run(c, m.optString("op"), m.optString("pkg"), m);
                r.put("ok", true);
            } catch (Throwable t) {
                Log.w(TAG, m.optString("op") + ": " + t);
                r = new JSONObject();
                try {
                    String why = t.getMessage();
                    r.put("ok", false).put("error", why == null ? t.getClass().getSimpleName() : why);
                } catch (JSONException ignored) {
                    // Only put() on a fresh object.
                }
            }
            try {
                HostLink.get().send(r.put("t", "keel_done").put("req", m.optLong("req")));
            } catch (JSONException ignored) {
                // Only put() on a fresh object.
            }
        });
    }

    private static JSONObject run(Context c, String op, String pkg, JSONObject m) throws Exception {
        LauncherApps launcher = c.getSystemService(LauncherApps.class);
        switch (op) {
            case "shortcuts":
                watch(launcher);
                return new JSONObject().put("shortcuts", shortcuts(launcher, pkg));
            case "start":
                launcher.startShortcut(pkg, m.optString("id"), null, null, Process.myUserHandle());
                return new JSONObject();
            case "secure":
                return new JSONObject().put("secure", secure(c, pkg));
            default:
                throw new IllegalArgumentException("unknown request");
        }
    }

    private static synchronized void watch(LauncherApps launcher) {
        if (sWatching)
            return;
        sWatching = true;
        launcher.registerCallback(new LauncherApps.Callback() {
            @Override
            public void onPackageRemoved(String pkg, UserHandle user) {
            }

            @Override
            public void onPackageAdded(String pkg, UserHandle user) {
            }

            @Override
            public void onPackageChanged(String pkg, UserHandle user) {
            }

            @Override
            public void onPackagesAvailable(String[] pkgs, UserHandle user, boolean replacing) {
            }

            @Override
            public void onPackagesUnavailable(String[] pkgs, UserHandle user, boolean replacing) {
            }

            @Override
            public void onShortcutsChanged(String pkg, List<ShortcutInfo> shortcuts, UserHandle user) {
                try {
                    HostLink.get().send(new JSONObject().put("t", "keel_changed").put("pkg", pkg));
                } catch (JSONException ignored) {
                    // Only put() on a fresh object.
                }
            }
        }, new Handler(Looper.getMainLooper()));
    }

    // The app's manifest shortcuts, then its dynamic ones, each in rank order.
    private static JSONArray shortcuts(LauncherApps launcher, String pkg) throws JSONException {
        JSONArray out = new JSONArray();
        for (int flag : new int[] {LauncherApps.ShortcutQuery.FLAG_MATCH_MANIFEST,
                LauncherApps.ShortcutQuery.FLAG_MATCH_DYNAMIC}) {
            List<ShortcutInfo> list = launcher.getShortcuts(
                    new LauncherApps.ShortcutQuery().setPackage(pkg).setQueryFlags(flag), Process.myUserHandle());
            if (list == null)
                continue;
            list = new ArrayList<>(list);
            list.sort((a, b) -> Integer.compare(a.getRank(), b.getRank()));
            for (ShortcutInfo s : list) {
                if (!s.isEnabled() || s.getShortLabel() == null || out.length() >= MAX_SHORTCUTS)
                    continue;
                out.put(new JSONObject().put("id", s.getId()).put("label", s.getShortLabel().toString())
                        .put("long", s.getLongLabel() == null ? "" : s.getLongLabel().toString()));
            }
        }
        return out;
    }

    // Android has no call for another app's window flags: the window
    // manager's dump (the system uid holds DUMP) lists them per window.
    private static boolean secure(Context c, String pkg) throws Exception {
        IBinder wm = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, "window");
        File f = new File(c.getCacheDir(), "windows.txt");
        try (ParcelFileDescriptor out = ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_CREATE
                | ParcelFileDescriptor.MODE_TRUNCATE | ParcelFileDescriptor.MODE_WRITE_ONLY)) {
            wm.dump(out.getFileDescriptor(), new String[] {"windows"});
        }
        String dump = new String(Files.readAllBytes(f.toPath()), StandardCharsets.UTF_8);
        f.delete();
        return secureIn(dump, pkg);
    }

    /** Whether any window of pkg that has a surface is FLAG_SECURE, from dumpsys window windows. */
    static boolean secureIn(String dump, String pkg) {
        boolean seen = false;
        for (String block : dump.split("\n  Window #")) {
            if (!block.contains(" package=" + pkg + " ") && !block.contains(" package=" + pkg + "\n"))
                continue;
            if (!block.contains("mHasSurface=true"))
                continue;
            seen = true;
            int at = block.indexOf(" fl=");
            if (at < 0)
                continue;
            for (String token : block.substring(at + 4).split("[\\s}]+")) {
                if (token.equals("SECURE"))
                    return true;
                if (token.contains("="))
                    break;
            }
        }
        if (!seen)
            throw new IllegalStateException("no window of " + pkg + " in the window manager's dump");
        return false;
    }
}
