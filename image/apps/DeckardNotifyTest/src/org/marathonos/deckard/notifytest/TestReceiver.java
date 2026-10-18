package org.marathonos.deckard.notifytest;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Person;
import android.app.RemoteInput;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

/**
 * Posts test notifications on a silent channel ("am broadcast -a
 * org.marathonos.deckard.notifytest.POST --es kind chat|progress|plain") and
 * logs what comes back: taps, actions and inline replies.
 */
public class TestReceiver extends BroadcastReceiver {
    static final String TAG = "DeckardTest";
    static final String ACTION = "org.marathonos.deckard.notifytest.POST";
    static final String CHANNEL = "silent";
    static final String KEY_REPLY = "reply";

    @Override
    public void onReceive(Context context, Intent intent) {
        NotificationManager nm = context.getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Silent tests",
                NotificationManager.IMPORTANCE_LOW));
        String kind = intent.getStringExtra("kind");
        String event = intent.getStringExtra("event");
        if (event != null) {
            Bundle results = RemoteInput.getResultsFromIntent(intent);
            CharSequence reply = results == null ? null : results.getCharSequence(KEY_REPLY);
            Log.i(TAG, "event " + event + (reply == null ? "" : " reply=\"" + reply + "\""));
            if (reply != null)
                nm.notify(1, chat(context, reply.toString()));
            return;
        }
        Log.i(TAG, "post " + kind);
        if ("chat".equals(kind)) {
            nm.notify(1, chat(context, null));
        } else if ("progress".equals(kind)) {
            progress(context, nm, 0);
        } else if ("cancel".equals(kind)) {
            nm.cancelAll();
        } else {
            nm.notify(3, new Notification.Builder(context, CHANNEL)
                    .setSmallIcon(android.R.drawable.ic_dialog_info)
                    .setContentTitle("Plain test")
                    .setContentText("Two actions & a <tag>")
                    .setContentIntent(open(context, "plain", 30))
                    .setAutoCancel(true)
                    .addAction(new Notification.Action.Builder(null, "First", event(context, "first", 31)).build())
                    .addAction(new Notification.Action.Builder(null, "Second", event(context, "second", 32)).build())
                    .build());
        }
    }

    private static PendingIntent open(Context context, String from, int code) {
        Intent i = new Intent(context, TestActivity.class).putExtra("from", from)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        return PendingIntent.getActivity(context, code, i,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    private static PendingIntent event(Context context, String name, int code) {
        Intent i = new Intent(ACTION).setClass(context, TestReceiver.class).putExtra("event", name);
        return PendingIntent.getBroadcast(context, code, i,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_MUTABLE);
    }

    private static Notification chat(Context context, String reply) {
        Person me = new Person.Builder().setName("Me").build();
        Person ann = new Person.Builder().setName("Ann").build();
        Notification.MessagingStyle style = new Notification.MessagingStyle(me)
                .setConversationTitle("Climbing")
                .setGroupConversation(true)
                .addMessage("Are we still on for Saturday?", System.currentTimeMillis() - 60000, ann)
                .addMessage("Bring the long rope", System.currentTimeMillis() - 30000, ann);
        if (reply != null)
            style.addMessage(reply, System.currentTimeMillis(), (Person) null);
        RemoteInput input = new RemoteInput.Builder(KEY_REPLY).setLabel("Message Ann").build();
        return new Notification.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_dialog_email)
                .setLargeIcon(Icon.createWithResource(context, android.R.drawable.ic_dialog_info))
                .setCategory(Notification.CATEGORY_MESSAGE)
                .setStyle(style)
                .setContentIntent(open(context, "chat", 10))
                .setAutoCancel(true)
                .addAction(new Notification.Action.Builder(null, "Reply", event(context, "reply", 11))
                        .addRemoteInput(input).build())
                .addAction(new Notification.Action.Builder(null, "Mark read", event(context, "markread", 12)).build())
                .build();
    }

    private static void progress(Context context, NotificationManager nm, int percent) {
        Notification.Builder b = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("test-file.bin")
                .setContentText(percent < 100 ? percent + "%" : "Done")
                .setOngoing(percent < 100)
                .setOnlyAlertOnce(true)
                .setContentIntent(open(context, "progress", 20));
        if (percent < 100)
            b.setProgress(100, percent, false);
        nm.notify(2, b.build());
        if (percent < 100)
            new Handler(Looper.getMainLooper()).postDelayed(
                    () -> progress(context, nm, percent + 10), 1000);
    }
}
