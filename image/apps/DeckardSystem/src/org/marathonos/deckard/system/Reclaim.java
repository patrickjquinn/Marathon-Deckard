package org.marathonos.deckard.system;

import android.app.ActivityManager;
import android.content.Context;
import android.os.Process;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * deckardd's answer to memory pressure, in place of lmkd (which cannot read
 * this kernel's /proc/zoneinfo and never kills): ends the least recently used
 * cached app processes (protocol/bridge.md, reclaim). Only cached processes
 * of installed apps are candidates, and an insecure killPids re-checks each
 * one's adj at kill time, so a process that came forward meanwhile lives.
 */
final class Reclaim {
    private static final String TAG = "DeckardReclaim";
    private static final ExecutorService WORKER = Executors.newSingleThreadExecutor();
    private static final int MAX_COUNT = 8;

    private Reclaim() {}

    static void handle(JSONObject m) {
        WORKER.execute(() -> {
            JSONObject r = new JSONObject();
            try {
                try {
                    r.put("ok", true).put("killed", run(Math.max(1, Math.min(MAX_COUNT, m.optInt("count", 1))),
                            m.optString("reason")));
                } catch (Throwable t) {
                    Log.w(TAG, "reclaim: " + t);
                    r.put("ok", false).put("error", String.valueOf(t));
                }
                HostLink.get().send(r.put("t", "reclaimed").put("id", m.optString("id")));
            } catch (JSONException ignored) {
                // Only put() on a fresh object.
            }
        });
    }

    private static JSONArray run(int count, String reason) throws Exception {
        Context c = (Context) Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null);
        List<ActivityManager.RunningAppProcessInfo> procs =
                c.getSystemService(ActivityManager.class).getRunningAppProcesses();
        List<ActivityManager.RunningAppProcessInfo> cached = new ArrayList<>();
        if (procs != null) {
            for (ActivityManager.RunningAppProcessInfo p : procs) {
                if (p.importance >= ActivityManager.RunningAppProcessInfo.IMPORTANCE_CACHED
                        && p.uid >= Process.FIRST_APPLICATION_UID && !ours(p))
                    cached.add(p);
            }
        }
        // The list runs most recent first; lru orders within the cached band.
        Collections.reverse(cached);
        cached.sort((a, b) -> Integer.compare(b.lru, a.lru));
        List<ActivityManager.RunningAppProcessInfo> victims = cached.subList(0, Math.min(count, cached.size()));
        JSONArray killed = new JSONArray();
        Object am = ActivityManager.class.getMethod("getService").invoke(null);
        Method killPids = am.getClass().getMethod("killPids", int[].class, String.class, boolean.class);
        for (ActivityManager.RunningAppProcessInfo p : victims) {
            if (!Boolean.TRUE.equals(killPids.invoke(am, new int[] {p.pid}, "deckard reclaim " + reason, false)))
                continue;
            killed.put(new JSONObject().put("pid", p.pid).put("process", p.processName));
            Log.i(TAG, "reclaimed " + p.processName + " (" + p.pid + ", lru " + p.lru + ") on " + reason);
        }
        return killed;
    }

    private static boolean ours(ActivityManager.RunningAppProcessInfo p) {
        if (p.pkgList == null)
            return false;
        for (String pkg : p.pkgList) {
            if (pkg.startsWith("org.marathonos.deckard."))
                return true;
        }
        return false;
    }
}
