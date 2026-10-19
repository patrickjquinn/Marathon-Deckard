package org.marathonos.deckard.system;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.IBinder;
import android.provider.OpenableColumns;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;

/**
 * Android's share chooser, answered by Marathon's share sheet (protocol/bridge.md,
 * Share): copies what the app shares into xfer/ and sends share_out, which
 * deckardd hands to org.sailfishos.share. Android's chooser never shows.
 * Choosers for anything but ACTION_SEND start their intent directly, as the
 * app that asked.
 */
public final class Chooser extends Activity {
    private static final String TAG = "DeckardChooser";
    private static final int MAX_FILES = 32;
    private static final long MAX_BYTES = 1L << 30;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Intent target = getIntent().getParcelableExtra(Intent.EXTRA_INTENT, Intent.class);
        if (state != null || target == null) {
            finish();
            return;
        }
        String action = target.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            // As the app that asked, not as the system uid.
            try {
                Activity.class.getMethod("startActivityAsCaller", Intent.class, Bundle.class, boolean.class, int.class)
                        .invoke(this, target, null, false, 0);
            } catch (ReflectiveOperationException | RuntimeException e) {
                Log.w(TAG, "chooser target: " + e);
            }
            finish();
            return;
        }
        // Who started the chooser, as Android recorded it: getReferrer() is the
        // app's to set. The copy reads as the system uid, so each stream must
        // be one that app may read itself.
        int uid = -1;
        String pkg = null;
        try {
            Object client = Class.forName("android.app.ActivityClient").getMethod("getInstance").invoke(null);
            Object token = Activity.class.getMethod("getActivityToken").invoke(this);
            uid = (Integer) client.getClass().getMethod("getLaunchedFromUid", IBinder.class).invoke(client, token);
            pkg = (String) client.getClass().getMethod("getLaunchedFromPackage", IBinder.class).invoke(client, token);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "launched from: " + e);
        }
        final int from = uid;
        final String fromPkg = pkg;
        Intent copy = new Intent(target);
        new Thread(() -> {
            send(fromPkg, from, copy);
            runOnUiThread(this::finish);
        }, "deckard-chooser").start();
    }

    private void send(String pkg, int uid, Intent target) {
        if (pkg == null || uid < 0) {
            Log.w(TAG, "a share without its app");
            return;
        }
        byte[] random = new byte[8];
        new SecureRandom().nextBytes(random);
        StringBuilder id = new StringBuilder();
        for (byte b : random)
            id.append(String.format(Locale.ROOT, "%02x", b));
        try {
            JSONArray files = new JSONArray();
            long bytes = 0;
            for (Uri uri : streams(target)) {
                if (files.length() >= MAX_FILES)
                    break;
                if (checkUriPermission(uri, -1, uid, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                        != PackageManager.PERMISSION_GRANTED) {
                    Log.w(TAG, pkg + " shared a stream it cannot read: " + uri.getAuthority());
                    continue;
                }
                String name = unique(files, safe(displayName(uri)));
                File f = Files.file(id.toString(), name);
                f.getParentFile().mkdirs();
                try (InputStream in = getContentResolver().openInputStream(uri);
                     OutputStream out = new FileOutputStream(f)) {
                    byte[] buffer = new byte[64 * 1024];
                    int n;
                    while (in != null && (n = in.read(buffer)) > 0) {
                        bytes += n;
                        if (bytes > MAX_BYTES)
                            throw new IllegalStateException("too large");
                        out.write(buffer, 0, n);
                    }
                }
                files.put(name);
            }
            CharSequence text = target.getCharSequenceExtra(Intent.EXTRA_TEXT);
            String title = target.getStringExtra(Intent.EXTRA_SUBJECT);
            JSONObject m = new JSONObject().put("t", "share_out").put("id", id.toString()).put("pkg", pkg)
                    .put("mime", target.getType() != null ? target.getType() : "")
                    .put("title", title != null ? title : "")
                    .put("text", text == null ? "" : text.length() > 65536 ? text.subSequence(0, 65536).toString() : text.toString())
                    .put("files", files);
            HostLink.get().send(m);
        } catch (Exception e) {
            Log.w(TAG, "share from " + pkg + ": " + e);
        }
    }

    private static Set<Uri> streams(Intent target) {
        Set<Uri> uris = new LinkedHashSet<>();
        if (Intent.ACTION_SEND_MULTIPLE.equals(target.getAction())) {
            List<Uri> list = target.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri.class);
            if (list != null)
                uris.addAll(list);
        } else {
            Uri one = target.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class);
            if (one != null)
                uris.add(one);
        }
        ClipData clip = target.getClipData();
        for (int i = 0; clip != null && i < clip.getItemCount(); i++) {
            if (clip.getItemAt(i).getUri() != null)
                uris.add(clip.getItemAt(i).getUri());
        }
        uris.removeIf(u -> !"content".equals(u.getScheme()));
        return uris;
    }

    private String displayName(Uri uri) {
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && c.getString(0) != null)
                return c.getString(0);
        } catch (RuntimeException ignored) {
            // A provider without the column: fall back to the path.
        }
        return uri.getLastPathSegment();
    }

    private static String safe(String name) {
        if (name == null)
            return "file";
        name = name.substring(name.lastIndexOf('/') + 1).replaceAll("[\\x00-\\x1f\\\\]", "").trim();
        if (name.length() > 200)
            name = name.substring(0, 200);
        return name.isEmpty() || name.equals(".") || name.equals("..") ? "file" : name;
    }

    private static String unique(JSONArray taken, String name) {
        List<String> names = new ArrayList<>();
        for (int i = 0; i < taken.length(); i++)
            names.add(taken.optString(i));
        String candidate = name;
        for (int n = 2; names.contains(candidate); n++)
            candidate = n + "-" + name;
        return candidate;
    }
}
