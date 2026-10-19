package org.marathonos.deckard.system;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Binder;
import android.os.Bundle;
import android.os.Process;
import android.os.ResultReceiver;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * Android's permission requests to Marathon's prompt (docs/permissions.md).
 * PermissionController's patched GrantPermissionsActivity calls "ask" with
 * the question Android would draw and a ResultReceiver; this sends perm_ask
 * to deckardd and hands its perm_answer back. Only PermissionController
 * may call it.
 */
public final class PermissionPrompts extends ContentProvider {
    interface Answer {
        void onAnswer(String answer, boolean precise);
    }

    private static final String CONTROLLER = "com.android.permissioncontroller";
    private static final SecureRandom RANDOM = new SecureRandom();
    private static final Map<String, Answer> WAITING = new HashMap<>();

    static String newId() {
        return String.format(Locale.ROOT, "%016x", RANDOM.nextLong());
    }

    /** Sends one question; false when deckardd isn't there to ask. */
    static boolean ask(String id, String pkg, List<String> perms, String kind, boolean once, String accuracy,
            boolean preciseDefault, Answer answer) {
        if (!HostLink.get().isConnected())
            return false;
        synchronized (WAITING) {
            WAITING.put(id, answer);
        }
        try {
            HostLink.get().send(new JSONObject().put("t", "perm_ask").put("id", id).put("pkg", pkg)
                    .put("perms", new JSONArray(perms)).put("kind", kind).put("once", once)
                    .put("accuracy", accuracy).put("preciseDefault", preciseDefault));
        } catch (JSONException e) {
            synchronized (WAITING) {
                WAITING.remove(id);
            }
            return false;
        }
        return true;
    }

    static void cancel(String id) {
        synchronized (WAITING) {
            if (WAITING.remove(id) == null)
                return;
        }
        try {
            HostLink.get().send(new JSONObject().put("t", "perm_cancel").put("id", id));
        } catch (JSONException ignored) {
            // Never thrown for these values.
        }
    }

    /** perm_answer from deckardd. */
    static void handle(JSONObject m) {
        answered(m.optString("id"), m.optString("answer", "cancel"), m.optBoolean("precise"));
    }

    /** The channel dropped: Android asks for itself. */
    static void linkLost() {
        List<String> ids;
        synchronized (WAITING) {
            ids = new ArrayList<>(WAITING.keySet());
        }
        for (String id : ids)
            answered(id, "unsupported", false);
    }

    private static void answered(String id, String answer, boolean precise) {
        Answer a;
        synchronized (WAITING) {
            a = WAITING.remove(id);
        }
        if (a != null)
            a.onAnswer(answer, precise);
    }

    @Override
    public Bundle call(String method, String arg, Bundle extras) {
        if (!fromController() || extras == null)
            throw new SecurityException("not PermissionController");
        if ("cancel".equals(method)) {
            cancel(extras.getString("id", ""));
            return null;
        }
        if (!"ask".equals(method))
            return null;
        ResultReceiver receiver = extras.getParcelable("receiver", ResultReceiver.class);
        ArrayList<String> perms = extras.getStringArrayList("perms");
        String pkg = extras.getString("pkg");
        if (receiver == null || perms == null || pkg == null)
            return null;
        String id = newId();
        boolean sent = ask(id, pkg, perms, extras.getString("kind", "foreground"), extras.getBoolean("once"),
                extras.getString("accuracy", ""), extras.getBoolean("preciseDefault"), (answer, precise) -> {
                    Bundle data = new Bundle();
                    data.putString("id", id);
                    data.putString("answer", answer);
                    data.putBoolean("precise", precise);
                    receiver.send(0, data);
                });
        if (!sent)
            return null;
        Bundle reply = new Bundle();
        reply.putString("id", id);
        return reply;
    }

    private boolean fromController() {
        int uid = Binder.getCallingUid();
        if (uid == Process.myUid())
            return true;
        try {
            return uid == getContext().getPackageManager().getPackageUid(CONTROLLER, PackageManager.PackageInfoFlags.of(0));
        } catch (PackageManager.NameNotFoundException e) {
            Log.w(HostLink.TAG, "no " + CONTROLLER);
            return false;
        }
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        return null;
    }

    @Override
    public String getType(Uri uri) {
        return null;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        return 0;
    }
}
