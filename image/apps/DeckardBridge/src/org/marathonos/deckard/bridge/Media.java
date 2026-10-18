package org.marathonos.deckard.bridge;

import android.content.ComponentName;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.media.MediaMetadata;
import android.media.session.MediaController;
import android.media.session.MediaSession;
import android.media.session.MediaSessionManager;
import android.media.session.PlaybackState;
import android.os.Handler;
import android.os.SystemClock;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/** Active MediaSessions, mirrored to deckardd as MPRIS players. */
final class Media implements MediaSessionManager.OnActiveSessionsChangedListener {
    private static final int MAX_ART = 512;

    private final Context mContext;
    private final Handler mMain;
    private final Link mLink;
    private final ComponentName mListener;
    private final MediaSessionManager mManager;
    private final Map<MediaSession.Token, Session> mSessions = new HashMap<>();
    private int mNextSid = 1;

    private final class Session extends MediaController.Callback {
        final int sid = mNextSid++;
        final MediaController controller;
        Bitmap art;
        boolean artSent;
        boolean scheduled;

        Session(MediaController controller) {
            this.controller = controller;
            controller.registerCallback(this, mMain);
        }

        @Override
        public void onPlaybackStateChanged(PlaybackState state) {
            schedule();
        }

        @Override
        public void onMetadataChanged(MediaMetadata metadata) {
            schedule();
        }

        @Override
        public void onSessionDestroyed() {
            remove(controller.getSessionToken());
        }

        void schedule() {
            if (scheduled)
                return;
            scheduled = true;
            mMain.postDelayed(this::send, 100);
        }

        void send() {
            scheduled = false;
            if (mSessions.get(controller.getSessionToken()) != this)
                return;
            try {
                mLink.send(describe());
            } catch (JSONException e) {
                Log.w(Link.TAG, "media " + sid + ": " + e.getMessage());
            }
        }

        JSONObject describe() throws JSONException {
            JSONObject m = new JSONObject();
            m.put("t", "media");
            m.put("sid", sid);
            String pkg = controller.getPackageName();
            m.put("pkg", pkg);
            m.put("label", label(pkg));
            PlaybackState ps = controller.getPlaybackState();
            String status = "Stopped";
            long position = 0;
            float speed = 1f;
            long actions = 0;
            if (ps != null) {
                switch (ps.getState()) {
                    case PlaybackState.STATE_PLAYING:
                    case PlaybackState.STATE_BUFFERING:
                    case PlaybackState.STATE_FAST_FORWARDING:
                    case PlaybackState.STATE_REWINDING:
                        status = "Playing";
                        break;
                    case PlaybackState.STATE_PAUSED:
                    case PlaybackState.STATE_CONNECTING:
                        status = "Paused";
                        break;
                    default:
                        break;
                }
                speed = ps.getPlaybackSpeed();
                position = ps.getPosition();
                if (status.equals("Playing") && ps.getLastPositionUpdateTime() > 0)
                    position += (long) ((SystemClock.elapsedRealtime() - ps.getLastPositionUpdateTime()) * speed);
                actions = ps.getActions();
            }
            m.put("state", status);
            m.put("position", Math.max(0, position));
            m.put("speed", (double) speed);
            JSONObject can = new JSONObject();
            can.put("play", (actions & (PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PLAY_PAUSE)) != 0);
            can.put("pause", (actions & (PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE)) != 0);
            can.put("next", (actions & PlaybackState.ACTION_SKIP_TO_NEXT) != 0);
            can.put("previous", (actions & PlaybackState.ACTION_SKIP_TO_PREVIOUS) != 0);
            can.put("seek", (actions & PlaybackState.ACTION_SEEK_TO) != 0);
            can.put("stop", (actions & PlaybackState.ACTION_STOP) != 0);
            m.put("can", can);

            MediaMetadata md = controller.getMetadata();
            if (md != null) {
                m.put("title", text(md, MediaMetadata.METADATA_KEY_TITLE, MediaMetadata.METADATA_KEY_DISPLAY_TITLE));
                m.put("artist", text(md, MediaMetadata.METADATA_KEY_ARTIST, MediaMetadata.METADATA_KEY_ALBUM_ARTIST));
                m.put("album", text(md, MediaMetadata.METADATA_KEY_ALBUM, null));
                m.put("duration", Math.max(0, md.getLong(MediaMetadata.METADATA_KEY_DURATION)));
                Bitmap bitmap = md.getBitmap(MediaMetadata.METADATA_KEY_ART);
                if (bitmap == null)
                    bitmap = md.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART);
                if (bitmap == null)
                    bitmap = md.getBitmap(MediaMetadata.METADATA_KEY_DISPLAY_ICON);
                boolean changed = bitmap == null ? art != null : art == null || !bitmap.sameAs(art);
                if (changed || !artSent) {
                    art = bitmap;
                    artSent = true;
                    m.put("art", bitmap == null ? "" : Png.encode(bitmap, MAX_ART));
                }
            }
            return m;
        }
    }

    Media(Context context, Handler main, Link link, ComponentName listener) {
        mContext = context;
        mMain = main;
        mLink = link;
        mListener = listener;
        mManager = context.getSystemService(MediaSessionManager.class);
        mManager.addOnActiveSessionsChangedListener(this, listener, main);
        onActiveSessionsChanged(mManager.getActiveSessions(listener));
    }

    void stop() {
        mManager.removeOnActiveSessionsChangedListener(this);
        for (Session s : mSessions.values())
            s.controller.unregisterCallback(s);
        mSessions.clear();
    }

    void resend() {
        for (Session s : mSessions.values()) {
            s.artSent = false;
            s.send();
        }
    }

    @Override
    public void onActiveSessionsChanged(List<MediaController> controllers) {
        List<MediaSession.Token> live = new ArrayList<>();
        if (controllers != null) {
            for (MediaController c : controllers) {
                live.add(c.getSessionToken());
                if (!mSessions.containsKey(c.getSessionToken())) {
                    Session s = new Session(c);
                    mSessions.put(c.getSessionToken(), s);
                    s.send();
                }
            }
        }
        for (MediaSession.Token token : new ArrayList<>(mSessions.keySet())) {
            if (!live.contains(token))
                remove(token);
        }
    }

    private void remove(MediaSession.Token token) {
        Session s = mSessions.remove(token);
        if (s == null)
            return;
        s.controller.unregisterCallback(s);
        try {
            mLink.send(new JSONObject().put("t", "media_gone").put("sid", s.sid));
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }

    void command(JSONObject m) {
        int sid = m.optInt("sid", -1);
        for (Session s : mSessions.values()) {
            if (s.sid != sid)
                continue;
            MediaController.TransportControls tc = s.controller.getTransportControls();
            switch (m.optString("cmd")) {
                case "play": tc.play(); break;
                case "pause": tc.pause(); break;
                case "playpause":
                    PlaybackState ps = s.controller.getPlaybackState();
                    if (ps != null && ps.getState() == PlaybackState.STATE_PLAYING)
                        tc.pause();
                    else
                        tc.play();
                    break;
                case "stop": tc.stop(); break;
                case "next": tc.skipToNext(); break;
                case "previous": tc.skipToPrevious(); break;
                case "seek": tc.seekTo(Math.max(0, m.optLong("position"))); break;
                default:
                    Log.w(Link.TAG, "unknown media command " + m.optString("cmd"));
                    return;
            }
            s.schedule();
            return;
        }
    }

    private String label(String pkg) {
        PackageManager pm = mContext.getPackageManager();
        try {
            return pm.getApplicationLabel(pm.getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0))).toString();
        } catch (PackageManager.NameNotFoundException e) {
            return pkg;
        }
    }

    private static String text(MediaMetadata md, String key, String fallback) {
        CharSequence value = md.getText(key);
        if ((value == null || value.length() == 0) && fallback != null)
            value = md.getText(fallback);
        return value == null ? "" : value.toString();
    }
}
