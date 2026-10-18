package org.marathonos.deckard.system;

import android.app.Activity;
import android.content.Intent;
import android.net.MailTo;
import android.net.Uri;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Hands a link, number, address, place or settings page to the Marathon app for it. */
public final class Forward extends Activity {
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        try {
            JSONObject m = describe(getIntent());
            if (m != null)
                HostLink.get().send(m.put("t", "open"));
        } catch (JSONException | RuntimeException e) {
            Log.w(HostLink.TAG, "not forwarded", e);
        }
        finish();
    }

    static JSONObject describe(Intent intent) throws JSONException {
        String action = intent.getAction();
        Uri data = intent.getData();
        String scheme = data != null ? data.getScheme() : null;
        JSONObject m = new JSONObject();
        if (action != null && action.startsWith("android.settings.")
                || Intent.ACTION_POWER_USAGE_SUMMARY.equals(action)) {
            m.put("kind", "settings").put("action", action);
            String pkg = intent.getStringExtra(Settings.EXTRA_APP_PACKAGE);
            if (pkg == null && data != null && "package".equals(scheme))
                pkg = data.getSchemeSpecificPart();
            if (pkg != null)
                m.put("pkg", pkg);
            return m;
        }
        if (Intent.ACTION_DIAL.equals(action) && data == null)
            return m.put("kind", "tel").put("uri", "tel:");
        if (scheme == null)
            return null;
        switch (scheme) {
            case "http":
            case "https":
                return m.put("kind", "url").put("uri", data.toString());
            case "tel":
                return m.put("kind", "tel").put("uri", data.toString());
            case "sms":
            case "smsto":
            case "mms":
            case "mmsto":
                m.put("kind", "sms").put("uri", data.toString());
                String body = intent.getStringExtra("sms_body");
                return body != null ? m.put("body", body) : m;
            case "geo":
                return m.put("kind", "geo").put("uri", data.toString());
            case "mailto":
                return mail(m, intent, data);
            default:
                return null;
        }
    }

    private static JSONObject mail(JSONObject m, Intent intent, Uri data) throws JSONException {
        JSONArray to = new JSONArray();
        JSONArray cc = new JSONArray();
        String subject = intent.getStringExtra(Intent.EXTRA_SUBJECT);
        String body = intent.getStringExtra(Intent.EXTRA_TEXT);
        if (MailTo.isMailTo(data.toString())) {
            MailTo mailTo = MailTo.parse(data.toString());
            addAll(to, mailTo.getTo());
            addAll(cc, mailTo.getCc());
            if (subject == null)
                subject = mailTo.getSubject();
            if (body == null)
                body = mailTo.getBody();
        }
        addAll(to, intent.getStringArrayExtra(Intent.EXTRA_EMAIL));
        addAll(cc, intent.getStringArrayExtra(Intent.EXTRA_CC));
        m.put("kind", "mailto").put("to", to).put("cc", cc);
        if (subject != null)
            m.put("subject", subject);
        if (body != null)
            m.put("body", body);
        return m;
    }

    private static void addAll(JSONArray list, String addresses) {
        if (addresses == null)
            return;
        for (String a : addresses.split(","))
            if (!a.trim().isEmpty())
                list.put(a.trim());
    }

    private static void addAll(JSONArray list, String[] addresses) {
        if (addresses == null)
            return;
        for (String a : addresses)
            addAll(list, a);
    }
}
