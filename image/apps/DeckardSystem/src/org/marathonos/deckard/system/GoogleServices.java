package org.marathonos.deckard.system;

import android.accounts.Account;
import android.accounts.AccountManager;
import android.content.ComponentName;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * microG for Marathon Settings (docs/microg.md): on or off, push messaging,
 * the Google accounts, and microG's own sign-in and self-check. microG's
 * settings provider is signature-protected; the system uid passes.
 */
final class GoogleServices {
    static final String GMS = "com.google.android.gms";
    private static final String COMPANION = "com.android.vending";
    private static final Uri GCM = Uri.parse("content://com.google.android.gms.microg.settings/gcm");
    private static final String ENABLE_GCM = "gcm_enable_mcs_service";
    private static final String ACCOUNT_TYPE = "com.google";

    private GoogleServices() {}

    /**
     * Background location is restricted: microG, added to an Android that
     * was set up before it, isn't exempt, so deckard-microg's pm grant needs
     * it allowlisted first.
     */
    static void prepare(Context c) {
        try {
            c.getPackageManager().addWhitelistedRestrictedPermission(GMS,
                    "android.permission.ACCESS_BACKGROUND_LOCATION", PackageManager.FLAG_PERMISSION_WHITELIST_SYSTEM);
        } catch (RuntimeException e) {
            // Not installed, or not ours to allowlist.
        }
    }

    static JSONObject info(Context c) throws JSONException {
        JSONObject r = new JSONObject();
        PackageManager pm = c.getPackageManager();
        boolean installed;
        boolean enabled = false;
        try {
            enabled = pm.getApplicationInfo(GMS, PackageManager.ApplicationInfoFlags.of(
                    PackageManager.MATCH_DISABLED_COMPONENTS)).enabled;
            installed = true;
        } catch (PackageManager.NameNotFoundException e) {
            installed = false;
        }
        r.put("installed", installed).put("enabled", enabled).put("push", enabled && push(c));
        JSONArray accounts = new JSONArray();
        if (enabled) {
            for (Account a : AccountManager.get(c).getAccountsByType(ACCOUNT_TYPE))
                accounts.put(a.name);
        }
        return r.put("accounts", accounts);
    }

    private static boolean push(Context c) {
        try (Cursor cursor = c.getContentResolver().query(GCM, new String[] {ENABLE_GCM}, null, null, null)) {
            return cursor != null && cursor.moveToFirst() && cursor.getInt(0) != 0;
        } catch (RuntimeException e) {
            return false;
        }
    }

    static void set(Context c, String key, boolean on) {
        PackageManager pm = c.getPackageManager();
        switch (key) {
            case "enabled": {
                int state = on ? PackageManager.COMPONENT_ENABLED_STATE_DEFAULT
                        : PackageManager.COMPONENT_ENABLED_STATE_DISABLED_USER;
                pm.setApplicationEnabledSetting(GMS, state, 0);
                try {
                    pm.setApplicationEnabledSetting(COMPANION, state, 0);
                } catch (IllegalArgumentException e) {
                    // Companion isn't installed.
                }
                if (on && push(c))
                    reconnect(c);
                return;
            }
            case "push": {
                ContentValues v = new ContentValues();
                v.put(ENABLE_GCM, on);
                c.getContentResolver().update(GCM, v, null, null);
                if (on) {
                    reconnect(c);
                } else {
                    c.stopService(new Intent().setComponent(new ComponentName(GMS, "org.microg.gms.gcm.McsService")));
                }
                return;
            }
            default:
                throw new IllegalArgumentException("unknown setting");
        }
    }

    /** Push connects again: microG connects for this even with push off. */
    private static void reconnect(Context c) {
        c.sendBroadcast(new Intent("org.microg.gms.gcm.FORCE_TRY_RECONNECT")
                .setComponent(new ComponentName(GMS, "org.microg.gms.gcm.TriggerReceiver")));
    }

    /** Runs on Manage's worker: adding an account waits for microG's answer. */
    static void open(Context c, String page) throws Exception {
        Intent i;
        switch (page) {
            case "account": {
                Bundle b = AccountManager.get(c).addAccount(ACCOUNT_TYPE, null, null, null, null, null, null)
                        .getResult();
                i = b.getParcelable(AccountManager.KEY_INTENT, Intent.class);
                if (i == null)
                    throw new IllegalStateException("microG offered no sign-in");
                break;
            }
            case "selfcheck":
                i = new Intent().setComponent(new ComponentName(GMS, "org.microg.gms.ui.SelfCheckFragment$AsActivity"));
                break;
            default:
                throw new IllegalArgumentException("unknown page");
        }
        c.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    }
}
