package org.marathonos.deckard.bridge;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Person;
import android.app.RemoteInput;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.media.AudioManager;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Parcelable;
import android.service.notification.NotificationListenerService;
import android.service.notification.StatusBarNotification;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Mirrors Android notifications and media sessions to deckardd, which posts
 * them to Marathon's Hub and MPRIS; carries taps, actions, replies and
 * dismissals back.
 */
public class BridgeService extends NotificationListenerService implements Link.Listener {
    private static final int MAX_ICON = 96;
    private static final int MAX_IMAGE = 128;
    private static final int MAX_MESSAGES = 10;
    private static final Set<String> SYSTEM_NOISE = new HashSet<>(Arrays.asList(
            "PHYSICAL_KEYBOARD", "DEVELOPER", "DEVELOPER_IMPORTANT", "USB", "FOREGROUND_SERVICE"));

    private final Handler mMain = new Handler(Looper.getMainLooper());
    private final Map<String, StatusBarNotification> mForwarded = new HashMap<>();
    private final Set<String> mIconsSent = new HashSet<>();
    private static BridgeService sInstance;
    private Link mLink;
    private Media mMedia;
    private Installer mInstaller;
    private Actions mActions;

    @Override
    public void onListenerConnected() {
        Log.i(Link.TAG, "listener connected");
        sInstance = this;
        mLink = new Link(mMain, this);
        mMedia = new Media(this, mMain, mLink, new ComponentName(this, BridgeService.class));
        mInstaller = new Installer(this, mMain, mLink);
        mActions = new Actions(this, mMain, mLink);
    }

    @Override
    public void onListenerDisconnected() {
        Log.i(Link.TAG, "listener disconnected");
        sInstance = null;
        mMedia.stop();
        mInstaller.stop();
        mActions.stop();
        mLink.stop();
        mForwarded.clear();
        requestRebind(new ComponentName(this, BridgeService.class));
    }

    @Override
    public void onConnected() {
        mIconsSent.clear();
        mForwarded.clear();
        try {
            mLink.send(new JSONObject().put("t", "hello").put("v", 1));
            JSONArray keys = new JSONArray();
            RankingMap ranking = getCurrentRanking();
            StatusBarNotification[] active = getActiveNotifications();
            if (active != null) {
                for (StatusBarNotification sbn : active) {
                    forward(sbn, ranking);
                    if (mForwarded.containsKey(sbn.getKey()))
                        keys.put(sbn.getKey());
                }
            }
            mLink.send(new JSONObject().put("t", "sync").put("keys", keys));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
        mMedia.resend();
        mActions.sendAll();
    }

    @Override
    public void onNotificationPosted(StatusBarNotification sbn, RankingMap ranking) {
        if (mLink != null)
            forward(sbn, ranking);
    }

    @Override
    public void onNotificationRemoved(StatusBarNotification sbn, RankingMap ranking, int reason) {
        if (mLink != null && mForwarded.remove(sbn.getKey()) != null)
            sendRemove(sbn.getKey());
    }

    /** From ToastWatcher, on the main thread like the listener callbacks. */
    static void toast(String pkg, String text) {
        BridgeService self = sInstance;
        if (self == null || self.mLink == null)
            return;
        try {
            self.sendIcon(pkg);
            self.mLink.send(new JSONObject().put("t", "toast").put("pkg", pkg)
                    .put("label", self.label(pkg)).put("text", text));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }

    private void sendIcon(String pkg) throws JSONException {
        if (!mIconsSent.add(pkg))
            return;
        String icon = appIcon(pkg);
        if (icon != null)
            mLink.send(new JSONObject().put("t", "icon").put("pkg", pkg).put("png", icon));
    }

    private void sendRemove(String key) {
        try {
            mLink.send(new JSONObject().put("t", "remove").put("key", key));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }

    private boolean wanted(StatusBarNotification sbn) {
        Notification n = sbn.getNotification();
        if (sbn.getPackageName().equals(getPackageName()))
            return false;
        // MediaStyle: shown by Marathon's media controls through MPRIS.
        if (n.extras.containsKey(Notification.EXTRA_MEDIA_SESSION))
            return false;
        if ("android".equals(sbn.getPackageName()) && SYSTEM_NOISE.contains(n.getChannelId()))
            return false;
        if ((n.flags & Notification.FLAG_GROUP_SUMMARY) != 0) {
            StatusBarNotification[] active = getActiveNotifications();
            for (StatusBarNotification other : active == null ? new StatusBarNotification[0] : active) {
                if (other != sbn && !other.getKey().equals(sbn.getKey())
                        && sbn.getGroupKey().equals(other.getGroupKey()))
                    return false;
            }
        }
        return true;
    }

    private void forward(StatusBarNotification sbn, RankingMap rankingMap) {
        String key = sbn.getKey();
        if (!wanted(sbn)) {
            if (mForwarded.remove(key) != null)
                sendRemove(key);
            return;
        }
        try {
            sendIcon(sbn.getPackageName());
            Ranking ranking = new Ranking();
            int importance = rankingMap != null && rankingMap.getRanking(key, ranking)
                    ? ranking.getImportance() : NotificationManager.IMPORTANCE_DEFAULT;
            mLink.send(describe(sbn, importance, mForwarded.containsKey(key)));
            mForwarded.put(key, sbn);
        } catch (JSONException e) {
            Log.w(Link.TAG, "post " + key + ": " + e.getMessage());
        }
    }

    private JSONObject describe(StatusBarNotification sbn, int importance, boolean update)
            throws JSONException {
        Notification n = sbn.getNotification();
        Bundle x = n.extras;
        JSONObject m = new JSONObject();
        m.put("t", "post");
        m.put("key", sbn.getKey());
        m.put("pkg", sbn.getPackageName());
        m.put("label", label(sbn.getPackageName()));
        m.put("when", n.when);

        CharSequence title = x.getCharSequence(Notification.EXTRA_TITLE_BIG);
        if (title == null || title.length() == 0)
            title = x.getCharSequence(Notification.EXTRA_TITLE);
        CharSequence text = x.getCharSequence(Notification.EXTRA_BIG_TEXT);
        if (text == null || text.length() == 0)
            text = x.getCharSequence(Notification.EXTRA_TEXT);
        CharSequence[] lines = x.getCharSequenceArray(Notification.EXTRA_TEXT_LINES);
        if ((text == null || text.length() == 0) && lines != null) {
            StringBuilder joined = new StringBuilder();
            for (CharSequence line : lines)
                joined.append(joined.length() > 0 ? "\n" : "").append(line);
            text = joined;
        }
        if ((title == null || title.length() == 0) && (text == null || text.length() == 0))
            text = x.getCharSequence(Notification.EXTRA_SUB_TEXT);
        m.put("title", str(title));
        m.put("text", str(text));

        boolean ongoing = (n.flags & (Notification.FLAG_ONGOING_EVENT | Notification.FLAG_NO_CLEAR)) != 0;
        m.put("ongoing", ongoing);
        m.put("autoCancel", (n.flags & Notification.FLAG_AUTO_CANCEL) != 0);
        boolean silent = importance < NotificationManager.IMPORTANCE_DEFAULT
                || (update && (n.flags & Notification.FLAG_ONLY_ALERT_ONCE) != 0);
        m.put("silent", silent);
        if (n.category != null)
            m.put("category", n.category);
        int urgency = 1;
        if (Notification.CATEGORY_ALARM.equals(n.category) || Notification.CATEGORY_CALL.equals(n.category)
                || n.fullScreenIntent != null)
            urgency = 2;
        else if (importance <= NotificationManager.IMPORTANCE_LOW)
            urgency = 0;
        m.put("urgency", urgency);
        m.put("content", n.contentIntent != null);

        JSONArray actions = new JSONArray();
        if (n.actions != null) {
            for (int i = 0; i < n.actions.length; i++) {
                Notification.Action a = n.actions[i];
                if (a.actionIntent == null || a.isContextual())
                    continue;
                RemoteInput reply = freeForm(a);
                if (reply != null && !m.has("reply")) {
                    CharSequence hint = reply.getLabel();
                    m.put("reply", new JSONObject().put("action", "a" + i).put("label", str(a.title))
                            .put("placeholder", hint == null ? "" : hint.toString()));
                    continue;
                }
                actions.put(new JSONObject().put("id", "a" + i).put("label", str(a.title)));
            }
        }
        m.put("actions", actions);

        int max = x.getInt(Notification.EXTRA_PROGRESS_MAX, 0);
        boolean indeterminate = x.getBoolean(Notification.EXTRA_PROGRESS_INDETERMINATE, false);
        if (max > 0 || indeterminate) {
            m.put("progress", new JSONObject().put("max", max)
                    .put("current", x.getInt(Notification.EXTRA_PROGRESS, 0))
                    .put("indeterminate", indeterminate));
        }

        JSONObject conversation = conversation(x);
        if (conversation != null)
            m.put("conversation", conversation);

        Icon large = n.getLargeIcon();
        if (large != null) {
            String png = Png.encode(large.loadDrawable(this), MAX_IMAGE);
            if (png != null)
                m.put("image", png);
        }
        return m;
    }

    private static RemoteInput freeForm(Notification.Action a) {
        RemoteInput[] inputs = a.getRemoteInputs();
        if (inputs == null)
            return null;
        for (RemoteInput input : inputs) {
            if (input.getAllowFreeFormInput())
                return input;
        }
        return null;
    }

    private JSONObject conversation(Bundle x) throws JSONException {
        Parcelable[] parcels = x.getParcelableArray(Notification.EXTRA_MESSAGES, Parcelable.class);
        if (parcels == null || parcels.length == 0)
            return null;
        List<Notification.MessagingStyle.Message> messages =
                Notification.MessagingStyle.Message.getMessagesFromBundleArray(parcels);
        JSONObject c = new JSONObject();
        c.put("title", str(x.getCharSequence(Notification.EXTRA_CONVERSATION_TITLE)));
        c.put("group", x.getBoolean(Notification.EXTRA_IS_GROUP_CONVERSATION, false));
        Person self = x.getParcelable(Notification.EXTRA_MESSAGING_PERSON, Person.class);
        c.put("self", self == null ? "" : str(self.getName()));
        JSONArray list = new JSONArray();
        for (int i = Math.max(0, messages.size() - MAX_MESSAGES); i < messages.size(); i++) {
            Notification.MessagingStyle.Message message = messages.get(i);
            Person sender = message.getSenderPerson();
            list.put(new JSONObject()
                    .put("sender", sender == null ? "" : str(sender.getName()))
                    .put("text", str(message.getText()))
                    .put("time", message.getTimestamp()));
        }
        c.put("messages", list);
        return c;
    }

    @Override
    public void onMessage(JSONObject m) {
        String type = m.optString("t");
        if (type.equals("hello"))
            return;
        if (type.equals("media_cmd")) {
            mMedia.command(m);
            return;
        }
        if (type.equals("inspect") || type.equals("install") || type.equals("uninstall")
                || type.equals("packages")) {
            mInstaller.handle(type, m);
            return;
        }
        if (type.equals("launch") || type.equals("screen_read") || type.equals("ui_act")) {
            mActions.handle(type, m);
            return;
        }
        if (type.equals("audio")) {
            applyAudio(m.optBoolean("quiet", true), m.optDouble("alarm", 0.9));
            return;
        }
        String key = m.optString("key");
        StatusBarNotification sbn = mForwarded.get(key);
        if (sbn == null) {
            Log.i(Link.TAG, type + " for unknown notification " + key);
            return;
        }
        Notification n = sbn.getNotification();
        switch (type) {
            case "invoke":
                invoke(sbn, m.optString("action"));
                break;
            case "reply":
                reply(n, m.optString("action"), m.optString("text"));
                break;
            case "dismiss":
                cancelNotification(key);
                break;
            default:
                Log.w(Link.TAG, "unknown message " + type);
        }
    }

    /**
     * Marathon plays notification sounds itself, following its profile and Do
     * Not Disturb; Android's own notification, ring and system sounds stay
     * off. Media plays at full volume under Marathon's output volume.
     */
    private void applyAudio(boolean quiet, double alarm) {
        AudioManager am = getSystemService(AudioManager.class);
        for (int stream : new int[] {AudioManager.STREAM_NOTIFICATION, AudioManager.STREAM_RING,
                AudioManager.STREAM_SYSTEM}) {
            setVolume(am, stream, 0);
        }
        setVolume(am, AudioManager.STREAM_MUSIC, am.getStreamMaxVolume(AudioManager.STREAM_MUSIC));
        int alarmMax = am.getStreamMaxVolume(AudioManager.STREAM_ALARM);
        setVolume(am, AudioManager.STREAM_ALARM,
                (int) Math.round(Math.max(0, Math.min(1, alarm)) * alarmMax));
        // After the volumes: a ringer-mode change from them turns Do Not Disturb off.
        mMain.postDelayed(() -> requestInterruptionFilter(
                quiet ? INTERRUPTION_FILTER_ALARMS : INTERRUPTION_FILTER_ALL), 200);
        Log.i(Link.TAG, "audio: " + (quiet ? "quiet" : "audible") + ", alarm " + alarm);
    }

    private static void setVolume(AudioManager am, int stream, int index) {
        try {
            am.setStreamVolume(stream, Math.max(am.getStreamMinVolume(stream), index), 0);
        } catch (SecurityException e) {
            Log.w(Link.TAG, "volume of stream " + stream + ": " + e.getMessage());
        }
    }

    private static Notification.Action action(Notification n, String id) {
        if (n.actions == null || !id.startsWith("a"))
            return null;
        try {
            int index = Integer.parseInt(id.substring(1));
            return index >= 0 && index < n.actions.length ? n.actions[index] : null;
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private void invoke(StatusBarNotification sbn, String id) {
        Notification n = sbn.getNotification();
        PendingIntent intent;
        if (id.equals("default")) {
            intent = n.contentIntent;
        } else {
            Notification.Action a = action(n, id);
            intent = a == null ? null : a.actionIntent;
        }
        if (intent == null)
            return;
        try {
            intent.send();
        } catch (PendingIntent.CanceledException e) {
            Log.i(Link.TAG, "cancelled intent for " + sbn.getKey());
        }
        if (id.equals("default") && (n.flags & Notification.FLAG_AUTO_CANCEL) != 0)
            cancelNotification(sbn.getKey());
    }

    private void reply(Notification n, String id, String text) {
        Notification.Action a = action(n, id);
        RemoteInput input = a == null ? null : freeForm(a);
        if (input == null || text.isEmpty())
            return;
        Intent fill = new Intent();
        Bundle results = new Bundle();
        results.putCharSequence(input.getResultKey(), text);
        RemoteInput.addResultsToIntent(new RemoteInput[] {input}, fill, results);
        RemoteInput.setResultsSource(fill, RemoteInput.SOURCE_FREE_FORM_INPUT);
        try {
            a.actionIntent.send(this, 0, fill);
        } catch (PendingIntent.CanceledException e) {
            Log.i(Link.TAG, "cancelled reply intent");
        }
    }

    private String label(String pkg) {
        PackageManager pm = getPackageManager();
        try {
            return pm.getApplicationLabel(pm.getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0))).toString();
        } catch (PackageManager.NameNotFoundException e) {
            return pkg;
        }
    }

    private String appIcon(String pkg) {
        try {
            return Png.encode(getPackageManager().getApplicationIcon(pkg), MAX_ICON);
        } catch (PackageManager.NameNotFoundException e) {
            return null;
        }
    }

    private static String str(CharSequence s) {
        return s == null ? "" : s.toString();
    }
}
