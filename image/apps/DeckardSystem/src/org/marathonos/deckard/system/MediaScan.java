package org.marathonos.deckard.system;

import android.content.ContentResolver;
import android.content.Context;
import android.provider.MediaStore;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Marathon's folders are Android's shared storage, but what the host adds
 * or removes there bypasses MediaProvider's FUSE daemon, so MediaProvider
 * doesn't know. deckardd names the folders that changed (media_scan) and
 * MediaProvider scans them, which picks up new files and forgets gone ones.
 */
final class MediaScan {
    private static final String TAG = "DeckardMediaScan";
    private static final String ROOT = "/storage/emulated/0/";
    private static final String[] SHARED = {"Documents", "Download", "Pictures", "Music", "Movies", "DCIM"};
    private static final ExecutorService WORKER = Executors.newSingleThreadExecutor();

    private MediaScan() {}

    static boolean isShared(String path) {
        if (path == null || !path.startsWith(ROOT) || path.contains("/../") || path.endsWith("/..")
                || path.contains("/./") || path.contains("//"))
            return false;
        String rest = path.substring(ROOT.length());
        for (String name : SHARED) {
            if (rest.equals(name) || rest.startsWith(name + "/"))
                return true;
        }
        return false;
    }

    static void handle(JSONObject m) {
        JSONArray paths = m.optJSONArray("paths");
        if (paths == null)
            return;
        WORKER.execute(() -> {
            try {
                ContentResolver resolver = ((Context) Class.forName("android.app.ActivityThread")
                        .getMethod("currentApplication").invoke(null)).getContentResolver();
                for (int i = 0; i < paths.length() && i < 64; i++) {
                    String path = paths.optString(i);
                    if (!isShared(path))
                        continue;
                    MediaStore.class.getMethod("scanFile", ContentResolver.class, File.class)
                            .invoke(null, resolver, new File(path));
                }
            } catch (ReflectiveOperationException | RuntimeException e) {
                Log.w(TAG, "scan: " + e);
            }
        });
    }
}
