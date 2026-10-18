package org.marathonos.deckard.notifytest;

import android.app.Activity;
import android.os.Bundle;
import android.view.KeyEvent;
import android.util.Log;
import android.widget.TextView;

/** Opened by a notification tap; logs how it was opened. */
public class TestActivity extends Activity {
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        String from = getIntent().getStringExtra("from");
        Log.i(TestReceiver.TAG, "activity opened from " + from);
        TextView text = new TextView(this);
        text.setTextSize(24);
        text.setPadding(48, 96, 48, 48);
        text.setText("Notify Test\nopened from: " + from);
        setContentView(text);
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        Log.i(TestReceiver.TAG, "key " + KeyEvent.keyCodeToString(event.getKeyCode())
                + (event.getAction() == KeyEvent.ACTION_DOWN ? " down" : " up"));
        return super.dispatchKeyEvent(event);
    }

    @Override
    protected void onDestroy() {
        Log.i(TestReceiver.TAG, "activity destroyed");
        super.onDestroy();
    }
}
