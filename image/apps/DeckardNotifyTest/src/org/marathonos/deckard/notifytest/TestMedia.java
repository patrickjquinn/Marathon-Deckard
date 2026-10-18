package org.marathonos.deckard.notifytest;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.SystemClock;
import android.util.Log;

/** A media session that plays nothing, for checking MPRIS without sound. */
final class TestMedia extends MediaSession.Callback {
    private static TestMedia sInstance;
    private final MediaSession mSession;
    private int mTrack = 1;
    private long mPosition;
    private boolean mPlaying = true;

    static void start(Context context) {
        if (sInstance == null)
            sInstance = new TestMedia(context.getApplicationContext());
    }

    static void stop() {
        if (sInstance != null) {
            sInstance.mSession.release();
            sInstance = null;
        }
    }

    private TestMedia(Context context) {
        mSession = new MediaSession(context, "DeckardTest");
        mSession.setCallback(this);
        publish();
        mSession.setActive(true);
    }

    private void publish() {
        Bitmap art = Bitmap.createBitmap(256, 256, Bitmap.Config.ARGB_8888);
        new Canvas(art).drawColor(mTrack % 2 == 1 ? Color.rgb(0, 150, 136) : Color.rgb(255, 152, 0));
        Paint p = new Paint();
        p.setColor(Color.WHITE);
        p.setTextSize(120);
        new Canvas(art).drawText(String.valueOf(mTrack), 90, 170, p);
        mSession.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, "Silent track " + mTrack)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, "Deckard")
                .putString(MediaMetadata.METADATA_KEY_ALBUM, "Bridge tests")
                .putLong(MediaMetadata.METADATA_KEY_DURATION, 200000)
                .putBitmap(MediaMetadata.METADATA_KEY_ART, art)
                .build());
        state();
    }

    private void state() {
        mSession.setPlaybackState(new PlaybackState.Builder()
                .setState(mPlaying ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        mPosition, 1f, SystemClock.elapsedRealtime())
                .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
                        | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_SKIP_TO_NEXT
                        | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SEEK_TO
                        | PlaybackState.ACTION_STOP)
                .build());
    }

    @Override
    public void onPlay() {
        Log.i(TestReceiver.TAG, "media play");
        mPlaying = true;
        state();
    }

    @Override
    public void onPause() {
        Log.i(TestReceiver.TAG, "media pause");
        mPlaying = false;
        state();
    }

    @Override
    public void onSkipToNext() {
        Log.i(TestReceiver.TAG, "media next");
        mTrack++;
        mPosition = 0;
        publish();
    }

    @Override
    public void onSkipToPrevious() {
        Log.i(TestReceiver.TAG, "media previous");
        mTrack = Math.max(1, mTrack - 1);
        mPosition = 0;
        publish();
    }

    @Override
    public void onSeekTo(long position) {
        Log.i(TestReceiver.TAG, "media seek " + position);
        mPosition = position;
        state();
    }

    @Override
    public void onStop() {
        Log.i(TestReceiver.TAG, "media stop");
        stop();
    }
}
