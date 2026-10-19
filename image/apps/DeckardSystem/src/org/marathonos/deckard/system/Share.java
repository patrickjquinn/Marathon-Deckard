package org.marathonos.deckard.system;

import android.content.ClipData;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import android.net.Uri;
import android.util.Base64;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Android apps as targets of Marathon's share sheet (protocol/bridge.md,
 * Share): lists the activities that take ACTION_SEND of a type, and starts
 * the chosen one with the text and the files deckardd put in xfer/.
 */
final class Share {
    private static final String TAG = "DeckardShare";
    private static final ExecutorService WORKER = Executors.newSingleThreadExecutor();
    private static final int ICON = 96;
    private static final int MAX_TARGETS = 64;

    private Share() {}

    static void handle(JSONObject m) {
        WORKER.execute(() -> {
            JSONObject r = new JSONObject();
            try {
                try {
                    if ("share_targets".equals(m.optString("t"))) {
                        r.put("t", "share_targets_result").put("targets", targets(m.optString("mime")));
                    } else {
                        start(m);
                        r.put("t", "shared").put("ok", true);
                    }
                } catch (Throwable t) {
                    Log.w(TAG, m.optString("t") + ": " + t);
                    r.put("t", "share_targets".equals(m.optString("t")) ? "share_targets_result" : "shared")
                            .put("ok", false).put("error", String.valueOf(t));
                }
                HostLink.get().send(r.put("id", m.optString("id")));
            } catch (JSONException ignored) {
                // Only put() on a fresh object.
            }
        });
    }

    private static Context context() throws Exception {
        return (Context) Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null);
    }

    private static List<ResolveInfo> query(PackageManager pm, String action, String mime) {
        return pm.queryIntentActivities(new Intent(action).setType(mime),
                PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_DEFAULT_ONLY));
    }

    private static JSONArray targets(String mime) throws Exception {
        PackageManager pm = context().getPackageManager();
        Set<String> multiple = new HashSet<>();
        for (ResolveInfo ri : query(pm, Intent.ACTION_SEND_MULTIPLE, mime))
            multiple.add(ri.activityInfo.packageName + "/" + ri.activityInfo.name);
        JSONArray out = new JSONArray();
        for (ResolveInfo ri : query(pm, Intent.ACTION_SEND, mime)) {
            String pkg = ri.activityInfo.packageName;
            if (!ri.activityInfo.exported || pkg.equals("android") || pkg.startsWith("org.marathonos.deckard."))
                continue;
            String id = pkg + "/" + ri.activityInfo.name;
            JSONObject t = new JSONObject().put("pkg", pkg).put("activity", ri.activityInfo.name)
                    .put("label", String.valueOf(ri.loadLabel(pm))).put("multiple", multiple.contains(id));
            String icon = png(ri.loadIcon(pm));
            if (icon != null)
                t.put("icon", icon);
            out.put(t);
            if (out.length() >= MAX_TARGETS)
                break;
        }
        return out;
    }

    private static String png(Drawable d) {
        if (d == null)
            return null;
        Bitmap b = Bitmap.createBitmap(ICON, ICON, Bitmap.Config.ARGB_8888);
        d.setBounds(0, 0, ICON, ICON);
        d.draw(new Canvas(b));
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        b.compress(Bitmap.CompressFormat.PNG, 100, out);
        return Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP);
    }

    private static void start(JSONObject m) throws Exception {
        String id = m.optString("id");
        String target = m.optString("target");
        int slash = target.indexOf('/');
        if (!Files.isRequestId(id) || slash <= 0)
            throw new IllegalArgumentException("bad share");
        ArrayList<Uri> streams = new ArrayList<>();
        JSONArray files = m.optJSONArray("files");
        for (int i = 0; files != null && i < files.length(); i++) {
            String name = files.optString(i);
            if (Files.file(id, name).isFile())
                streams.add(Files.uri(id, name));
        }
        String text = m.optString("text");
        String title = m.optString("title");
        Intent send = new Intent(streams.size() > 1 ? Intent.ACTION_SEND_MULTIPLE : Intent.ACTION_SEND)
                .setComponent(new ComponentName(target.substring(0, slash), target.substring(slash + 1)))
                .setType(m.optString("mime", "text/plain"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_GRANT_READ_URI_PERMISSION);
        if (!text.isEmpty())
            send.putExtra(Intent.EXTRA_TEXT, text);
        if (!title.isEmpty())
            send.putExtra(Intent.EXTRA_SUBJECT, title);
        if (streams.size() == 1)
            send.putExtra(Intent.EXTRA_STREAM, streams.get(0));
        else if (streams.size() > 1)
            send.putParcelableArrayListExtra(Intent.EXTRA_STREAM, streams);
        if (!streams.isEmpty()) {
            ClipData clip = ClipData.newRawUri(null, streams.get(0));
            for (int i = 1; i < streams.size(); i++)
                clip.addItem(new ClipData.Item(streams.get(i)));
            send.setClipData(clip);
        }
        context().startActivity(send);
    }
}
