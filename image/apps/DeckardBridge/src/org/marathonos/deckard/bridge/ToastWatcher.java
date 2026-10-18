package org.marathonos.deckard.bridge;

import android.accessibilityservice.AccessibilityService;
import android.view.accessibility.AccessibilityEvent;
import android.widget.Toast;

import java.util.List;

/**
 * Android's toasts as Marathon shows them. Waydroid maps no window for a
 * toast, but ToastPresenter announces each one to accessibility with its text
 * and package; only those events are subscribed to.
 */
public class ToastWatcher extends AccessibilityService {
    private static final String TOAST = Toast.class.getName();

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        if (event.getEventType() != AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED
                || event.getClassName() == null || !TOAST.contentEquals(event.getClassName())
                || event.getPackageName() == null)
            return;
        List<CharSequence> parts = event.getText();
        StringBuilder text = new StringBuilder();
        for (CharSequence part : parts)
            text.append(text.length() > 0 ? "\n" : "").append(part);
        if (text.length() > 0)
            BridgeService.toast(event.getPackageName().toString(), text.toString());
    }

    @Override
    public void onInterrupt() {
    }
}
