package org.marathonos.deckard.system;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.os.Bundle;
import android.provider.MediaStore;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Locale;

/**
 * Open, save and capture requests (file pickers, the camera intents): asks
 * Marathon through deckardd, which shows its own file chooser or camera, then
 * answers the app with files from {@link Files}.
 */
public final class Pick extends Activity {
    private String mId;
    private String mMode;
    private boolean mDone;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        if (state != null) {
            // The request belonged to an instance that is gone.
            finishWith(RESULT_CANCELED, null);
            return;
        }
        Intent in = getIntent();
        String action = in.getAction();
        byte[] random = new byte[8];
        new SecureRandom().nextBytes(random);
        StringBuilder id = new StringBuilder();
        for (byte b : random)
            id.append(String.format(Locale.ROOT, "%02x", b));
        mId = id.toString();
        try {
            JSONObject m = new JSONObject().put("t", "pick").put("id", mId);
            if (MediaStore.ACTION_IMAGE_CAPTURE.equals(action) || MediaStore.ACTION_VIDEO_CAPTURE.equals(action)) {
                mMode = "capture";
                m.put("kind", MediaStore.ACTION_VIDEO_CAPTURE.equals(action) ? "video" : "image");
            } else if (Intent.ACTION_CREATE_DOCUMENT.equals(action)) {
                mMode = "save";
                String title = in.getStringExtra(Intent.EXTRA_TITLE);
                m.put("name", title != null ? title : "Untitled");
            } else {
                mMode = "open";
                m.put("multiple", in.getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE, false));
            }
            m.put("mode", mMode);
            JSONArray mime = new JSONArray();
            if (in.getType() != null)
                mime.put(in.getType());
            String[] extra = in.getStringArrayExtra(Intent.EXTRA_MIME_TYPES);
            if (extra != null)
                for (String t : extra)
                    mime.put(t);
            m.put("mime", mime);
            HostLink.get().request(mId, m, this::onReply);
        } catch (JSONException e) {
            finishWith(RESULT_CANCELED, null);
        }
    }

    @Override
    public void onBackPressed() {
        cancel();
    }

    @Override
    protected void onDestroy() {
        if (!mDone && mId != null)
            cancel();
        super.onDestroy();
    }

    private void cancel() {
        HostLink.get().forget(mId);
        try {
            HostLink.get().send(new JSONObject().put("t", "pick_cancel").put("id", mId));
        } catch (JSONException ignored) {
            // A literal object.
        }
        finishWith(RESULT_CANCELED, null);
    }

    private void finishWith(int code, Intent data) {
        mDone = true;
        setResult(code, data);
        finish();
    }

    private void onReply(JSONObject m) {
        if (mDone)
            return;
        JSONArray files = m != null && m.optBoolean("ok") ? m.optJSONArray("files") : null;
        if (files == null || files.length() == 0) {
            if (m == null)
                Log.w(HostLink.TAG, "no host: " + mMode + " cancelled");
            finishWith(RESULT_CANCELED, null);
            return;
        }
        List<String> names = new ArrayList<>();
        for (int i = 0; i < files.length(); i++) {
            String name = files.optJSONObject(i) != null ? files.optJSONObject(i).optString("name") : "";
            try {
                Files.file(mId, name);
                names.add(name);
            } catch (IOException e) {
                Log.w(HostLink.TAG, "bad name from host: " + name);
            }
        }
        if (names.isEmpty()) {
            finishWith(RESULT_CANCELED, null);
            return;
        }
        switch (mMode) {
            case "save":
                answerSave(names.get(0));
                break;
            case "capture":
                answerCapture(names.get(0));
                break;
            default:
                answerOpen(names);
        }
    }

    private void answerOpen(List<String> names) {
        boolean document = Intent.ACTION_OPEN_DOCUMENT.equals(getIntent().getAction());
        Intent result = new Intent();
        Uri first = Files.uri(mId, names.get(0));
        result.setData(first);
        if (names.size() > 1 || getIntent().getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE, false)) {
            ClipData clip = ClipData.newRawUri(null, first);
            for (int i = 1; i < names.size(); i++)
                clip.addItem(new ClipData.Item(Files.uri(mId, names.get(i))));
            result.setClipData(clip);
        }
        result.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                | (document ? Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION : 0));
        if (!grant(names, false, document))
            return;
        finishWith(RESULT_OK, result);
    }

    // The flags on a result stay, for apps that read them to take a
    // persistable grant; the grant itself comes from Handoff.
    private boolean grant(List<String> names, boolean write, boolean persist) {
        try {
            Files.grant(this, getCallingPackage(), mId, names, write, persist);
            return true;
        } catch (IOException | RuntimeException e) {
            Log.w(HostLink.TAG, "grant to " + getCallingPackage(), e);
            finishWith(RESULT_CANCELED, null);
            return false;
        }
    }

    private void answerSave(String name) {
        try {
            File f = Files.file(mId, name);
            f.getParentFile().mkdirs();
            f.createNewFile();
        } catch (IOException e) {
            Log.w(HostLink.TAG, "save target", e);
            finishWith(RESULT_CANCELED, null);
            return;
        }
        Intent result = new Intent().setData(Files.uri(mId, name));
        result.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        if (!grant(Collections.singletonList(name), true, true))
            return;
        finishWith(RESULT_OK, result);
    }

    private void answerCapture(String name) {
        Uri output = getIntent().getParcelableExtra(MediaStore.EXTRA_OUTPUT);
        File f;
        try {
            f = Files.file(mId, name);
        } catch (IOException e) {
            finishWith(RESULT_CANCELED, null);
            return;
        }
        if (output != null) {
            try (InputStream from = new FileInputStream(f);
                 OutputStream to = getContentResolver().openOutputStream(output, "wt")) {
                byte[] buffer = new byte[64 * 1024];
                int n;
                while ((n = from.read(buffer)) > 0)
                    to.write(buffer, 0, n);
            } catch (IOException | RuntimeException e) {
                Log.w(HostLink.TAG, "capture output", e);
                finishWith(RESULT_CANCELED, null);
                return;
            }
            finishWith(RESULT_OK, new Intent().setData(output));
            return;
        }
        Intent result = new Intent();
        if (MediaStore.ACTION_IMAGE_CAPTURE.equals(getIntent().getAction())) {
            BitmapFactory.Options o = new BitmapFactory.Options();
            o.inJustDecodeBounds = true;
            BitmapFactory.decodeFile(f.getPath(), o);
            o.inJustDecodeBounds = false;
            o.inSampleSize = Math.max(1, Math.max(o.outWidth, o.outHeight) / 512);
            Bitmap small = BitmapFactory.decodeFile(f.getPath(), o);
            if (small != null)
                result.putExtra("data", small);
        } else {
            result.setData(Files.uri(mId, name)).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            if (!grant(Collections.singletonList(name), false, false))
                return;
        }
        finishWith(RESULT_OK, result);
    }
}
