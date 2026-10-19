package org.marathonos.deckard.system;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.content.pm.VersionedPackage;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * Removes an app the person installed (protocol/bridge.md, Packages). Android
 * only uninstalls silently for the app's installer of record or the system
 * uid, so DeckardBridge cannot remove apps it did not install; this does.
 */
final class Uninstall {
    private static final String TAG = "DeckardUninstall";
    private static final String ACTION = "org.marathonos.deckard.system.UNINSTALLED";
    private static boolean sRegistered;

    private Uninstall() {}

    static synchronized void handle(JSONObject m) {
        String id = m.optString("id");
        String pkg = m.optString("pkg");
        try {
            Context c = (Context) Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null);
            if (!sRegistered) {
                c.registerReceiver(new BroadcastReceiver() {
                    @Override
                    public void onReceive(Context context, Intent intent) {
                        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
                        String message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
                        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION)
                            message = "Android asked for a confirmation";
                        reply(intent.getStringExtra("id"), status == PackageInstaller.STATUS_SUCCESS,
                                intent.getStringExtra("pkg"), message);
                    }
                }, new IntentFilter(ACTION), Context.RECEIVER_NOT_EXPORTED);
                sRegistered = true;
            }
            ApplicationInfo ai = c.getPackageManager().getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0));
            if ((ai.flags & ApplicationInfo.FLAG_SYSTEM) != 0 || pkg.startsWith("org.marathonos.deckard.")) {
                reply(id, false, pkg, "part of the system");
                return;
            }
            Intent intent = new Intent(ACTION).setPackage(c.getPackageName()).putExtra("id", id).putExtra("pkg", pkg);
            PendingIntent sender = PendingIntent.getBroadcast(c, id.hashCode(), intent,
                    PendingIntent.FLAG_MUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
            c.getPackageManager().getPackageInstaller().uninstall(
                    new VersionedPackage(pkg, PackageManager.VERSION_CODE_HIGHEST), sender.getIntentSender());
        } catch (PackageManager.NameNotFoundException e) {
            reply(id, false, pkg, "not installed");
        } catch (Throwable t) {
            Log.w(TAG, "uninstall " + pkg + ": " + t);
            reply(id, false, pkg, String.valueOf(t));
        }
    }

    private static void reply(String id, boolean ok, String pkg, String message) {
        try {
            HostLink.get().send(new JSONObject().put("t", "install_result").put("id", id).put("ok", ok)
                    .put("pkg", pkg == null ? "" : pkg).put("message", message == null ? "" : message));
        } catch (JSONException ignored) {
            // Only put() on a fresh object.
        }
    }
}
