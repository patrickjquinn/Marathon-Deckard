package org.marathonos.deckard.bridge;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Installs, inspects and lists apps for deckardd through PackageInstaller
 * sessions. INSTALL_PACKAGES makes them silent: Android's installer UI never
 * shows. DeckardSystem removes apps (protocol/bridge.md).
 */
final class Installer {
    private static final String ACTION_RESULT = "org.marathonos.deckard.bridge.INSTALL_RESULT";
    private static final String DIR = "/data/deckard/install/";
    private static final int MAX_ICON = 128;

    private final Context mContext;
    private final Handler mMain;
    private final Link mLink;
    private final ExecutorService mWorker = Executors.newSingleThreadExecutor();
    private final BroadcastReceiver mChanges = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            onChange(intent);
        }
    };
    private final BroadcastReceiver mResults = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            onResult(intent);
        }
    };

    Installer(Context context, Handler main, Link link) {
        mContext = context;
        mMain = main;
        mLink = link;
        context.registerReceiver(mResults, new IntentFilter(ACTION_RESULT), null, main,
                Context.RECEIVER_NOT_EXPORTED);
        IntentFilter changes = new IntentFilter();
        changes.addAction(Intent.ACTION_PACKAGE_ADDED);
        changes.addAction(Intent.ACTION_PACKAGE_REMOVED);
        changes.addDataScheme("package");
        context.registerReceiver(mChanges, changes, null, main);
    }

    // A replace sends REMOVED then ADDED, both with EXTRA_REPLACING.
    private void onChange(Intent intent) {
        if (intent.getData() == null)
            return;
        boolean replacing = intent.getBooleanExtra(Intent.EXTRA_REPLACING, false);
        String change;
        if (Intent.ACTION_PACKAGE_ADDED.equals(intent.getAction()))
            change = replacing ? "updated" : "installed";
        else if (!replacing)
            change = "removed";
        else
            return;
        try {
            reply(new JSONObject().put("t", "package_changed")
                    .put("pkg", intent.getData().getSchemeSpecificPart()).put("change", change));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }

    void stop() {
        mContext.unregisterReceiver(mResults);
        mContext.unregisterReceiver(mChanges);
        mWorker.shutdownNow();
    }

    void handle(String type, JSONObject m) {
        String id = m.optString("id");
        try {
            switch (type) {
                case "inspect": reply(inspect(id, m.optString("path"))); break;
                case "install": install(id, m.optString("path")); break;
                case "packages": reply(packages(id)); break;
                case "device":
                    reply(Requirements.device(mContext.getPackageManager()).put("t", "device_result").put("id", id));
                    break;
                default: break;
            }
        } catch (JSONException e) {
            Log.w(Link.TAG, type + ": " + e.getMessage());
        }
    }

    // Only files deckardd placed in /data/deckard/install, and no "..".
    private static File apk(String path) {
        File f = new File(path);
        if (!path.startsWith(DIR) || path.contains("/../") || !f.getName().endsWith(".apk")
                || !f.isFile())
            return null;
        return f;
    }

    private void reply(JSONObject m) {
        mLink.send(m);
    }

    private JSONObject inspect(String id, String path) throws JSONException {
        JSONObject r = new JSONObject().put("t", "inspect_result").put("id", id);
        File f = apk(path);
        PackageManager pm = mContext.getPackageManager();
        PackageInfo info = f == null ? null : pm.getPackageArchiveInfo(f.getPath(),
                PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS
                        | PackageManager.GET_CONFIGURATIONS | PackageManager.GET_META_DATA));
        if (info == null || info.applicationInfo == null)
            return r.put("ok", false).put("message", "not an Android app");
        info.applicationInfo.sourceDir = f.getPath();
        info.applicationInfo.publicSourceDir = f.getPath();
        r.put("ok", true).put("pkg", info.packageName)
                .put("label", pm.getApplicationLabel(info.applicationInfo).toString())
                .put("versionName", info.versionName == null ? "" : info.versionName)
                .put("versionCode", info.getLongVersionCode())
                .put("minSdk", info.applicationInfo.minSdkVersion)
                .put("targetSdk", info.applicationInfo.targetSdkVersion);
        JSONArray perms = new JSONArray();
        if (info.requestedPermissions != null) {
            for (String p : info.requestedPermissions)
                perms.put(p);
        }
        r.put("permissions", perms);
        Requirements.of(pm, info, f, r);
        String icon = Png.encode(pm.getApplicationIcon(info.applicationInfo), MAX_ICON);
        if (icon != null)
            r.put("icon", icon);
        try {
            PackageInfo installed = pm.getPackageInfo(info.packageName, PackageManager.PackageInfoFlags.of(0));
            r.put("installedVersionCode", installed.getLongVersionCode())
                    .put("installedVersionName", installed.versionName == null ? "" : installed.versionName);
        } catch (PackageManager.NameNotFoundException e) {
            // A new install.
        }
        return r;
    }

    private void install(String id, String path) throws JSONException {
        File f = apk(path);
        if (f == null) {
            reply(result(id, false, "", "not an Android app"));
            return;
        }
        mWorker.execute(() -> {
            PackageInstaller pi = mContext.getPackageManager().getPackageInstaller();
            PackageInstaller.SessionParams params =
                    new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            params.setInstallReason(PackageManager.INSTALL_REASON_USER);
            try {
                int sid = pi.createSession(params);
                try (PackageInstaller.Session s = pi.openSession(sid)) {
                    long total = f.length(), done = 0;
                    int lastPercent = -1;
                    try (InputStream in = new FileInputStream(f);
                            OutputStream out = s.openWrite("base.apk", 0, total)) {
                        byte[] buf = new byte[256 * 1024];
                        for (int n; (n = in.read(buf)) > 0; ) {
                            out.write(buf, 0, n);
                            done += n;
                            int percent = (int) (done * 100 / Math.max(1, total));
                            if (percent / 10 != lastPercent / 10) {
                                lastPercent = percent;
                                progress(id, percent);
                            }
                        }
                        s.fsync(out);
                    }
                    Intent intent = new Intent(ACTION_RESULT).setPackage(mContext.getPackageName())
                            .putExtra("id", id);
                    PendingIntent sender = PendingIntent.getBroadcast(mContext, sid, intent,
                            PendingIntent.FLAG_MUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
                    s.commit(sender.getIntentSender());
                }
            } catch (IOException | RuntimeException e) {
                Log.w(Link.TAG, "install " + id + ": " + e);
                mMain.post(() -> sendResult(id, false, "", e.getMessage()));
            }
        });
    }

    private JSONObject packages(String id) throws JSONException {
        PackageManager pm = mContext.getPackageManager();
        JSONArray list = new JSONArray();
        for (PackageInfo info : pm.getInstalledPackages(PackageManager.PackageInfoFlags.of(0))) {
            if (info.applicationInfo == null)
                continue;
            list.put(new JSONObject().put("pkg", info.packageName)
                    .put("system", (info.applicationInfo.flags & ApplicationInfo.FLAG_SYSTEM) != 0)
                    .put("launchable", pm.getLaunchIntentForPackage(info.packageName) != null)
                    .put("label", pm.getApplicationLabel(info.applicationInfo).toString())
                    .put("versionName", info.versionName == null ? "" : info.versionName)
                    .put("versionCode", info.getLongVersionCode()));
        }
        return new JSONObject().put("t", "packages_result").put("id", id).put("packages", list);
    }

    private void progress(String id, int percent) {
        mMain.post(() -> {
            try {
                reply(new JSONObject().put("t", "install_progress").put("id", id).put("percent", percent));
            } catch (JSONException e) {
                Log.w(Link.TAG, e.getMessage());
            }
        });
    }

    private void onResult(Intent intent) {
        String id = intent.getStringExtra("id");
        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        String pkg = intent.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME);
        String message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION)
            message = "Android asked for a confirmation";
        sendResult(id, status == PackageInstaller.STATUS_SUCCESS, pkg, message);
    }

    private void sendResult(String id, boolean ok, String pkg, String message) {
        try {
            reply(result(id, ok, pkg, message));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }

    private static JSONObject result(String id, boolean ok, String pkg, String message) throws JSONException {
        return new JSONObject().put("t", "install_result").put("id", id).put("ok", ok)
                .put("pkg", pkg == null ? "" : pkg).put("message", message == null ? "" : message);
    }
}
