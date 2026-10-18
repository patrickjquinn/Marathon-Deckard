package org.marathonos.deckard.ime;

import android.inputmethodservice.InputMethodService;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.util.Log;
import android.view.KeyEvent;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.SurroundingText;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * Marathon's keyboard for Android apps: the shell draws it, this reports the
 * focused editor and applies what is typed. Its window is invisible.
 */
public final class DeckardIme extends InputMethodService implements Bridge.Listener {
    private static final int SURROUNDING = 256;

    private final Handler main = new Handler(Looper.getMainLooper());
    private Bridge bridge;
    private int coveredPx;
    private boolean editing;
    private int selStart = -1;
    private int selEnd = -1;
    private ContentType contentType = ContentType.of(InputType.TYPE_CLASS_TEXT);

    @Override
    public void onCreate() {
        super.onCreate();
        final Window window = getWindow().getWindow();
        if (window != null) {
            final WindowManager.LayoutParams lp = window.getAttributes();
            lp.alpha = 0f;
            window.setAttributes(lp);
            window.addFlags(WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE);
        }
        bridge = new Bridge(this, main);
        bridge.start();
    }

    @Override
    public void onDestroy() {
        bridge.stop();
        super.onDestroy();
    }

    @Override
    public View onCreateInputView() {
        return new View(this);
    }

    @Override
    public boolean onEvaluateFullscreenMode() {
        return false;
    }

    @Override
    public boolean onEvaluateInputViewShown() {
        return true;
    }

    // Insets from the covered height alone, never from the view tree.
    @Override
    public void onComputeInsets(Insets outInsets) {
        super.onComputeInsets(outInsets);
        final int top = Math.max(0, getWindow().getWindow().getDecorView().getHeight() - coveredPx);
        outInsets.contentTopInsets = top;
        outInsets.visibleTopInsets = top;
        outInsets.touchableInsets = Insets.TOUCHABLE_INSETS_REGION;
        outInsets.touchableRegion.setEmpty();
    }

    @Override
    public boolean onShowInputRequested(int flags, boolean configChange) {
        if (editing)
            bridge.send(message("show"));
        return true;
    }

    @Override
    public void hideWindow() {
        super.hideWindow();
        if (editing)
            bridge.send(message("hide"));
    }

    @Override
    public void onStartInput(EditorInfo attribute, boolean restarting) {
        super.onStartInput(attribute, restarting);
        editing = true;
        selStart = attribute.initialSelStart;
        selEnd = attribute.initialSelEnd;
        contentType = ContentType.of(attribute.inputType);
        sendStart(attribute);
        sendState();
    }

    @Override
    public void onFinishInput() {
        super.onFinishInput();
        if (editing)
            bridge.send(message("finish"));
        editing = false;
    }

    @Override
    public void onUpdateSelection(int oldSelStart, int oldSelEnd, int newSelStart, int newSelEnd,
            int candidatesStart, int candidatesEnd) {
        super.onUpdateSelection(oldSelStart, oldSelEnd, newSelStart, newSelEnd,
                candidatesStart, candidatesEnd);
        selStart = newSelStart;
        selEnd = newSelEnd;
        sendState();
    }

    @Override
    public void onConnected() {
        if (!editing)
            return;
        final EditorInfo info = getCurrentInputEditorInfo();
        if (info != null)
            sendStart(info);
        sendState();
        if (isInputViewShown())
            bridge.send(message("show"));
    }

    @Override
    public void onMessage(JSONObject m) {
        switch (m.optString("t")) {
            case "commit":
                commit(m.optString("text"), m.optInt("before"), m.optInt("after"));
                break;
            case "key":
                key(m.optString("key"));
                break;
            case "panel":
                panel(m.optInt("covered"), m.optInt("height"));
                break;
            default:
                break;
        }
    }

    // A replacement selects the text and types over it in one edit, so the
    // field never passes through the shorter text (Launcher's search resets
    // itself when it goes empty).
    private void commit(String text, int before, int after) {
        final InputConnection ic = getCurrentInputConnection();
        if (ic == null)
            return;
        before = Math.max(0, Math.min(before, SURROUNDING));
        after = Math.max(0, Math.min(after, SURROUNDING));
        ic.beginBatchEdit();
        if (before > 0 || after > 0) {
            if (selStart >= before && selStart == selEnd)
                ic.setSelection(selStart - before, selEnd + after);
            else
                ic.deleteSurroundingText(before, after);
        }
        ic.commitText(text, 1);
        ic.endBatchEdit();
    }

    private void key(String name) {
        final InputConnection ic = getCurrentInputConnection();
        if (ic == null)
            return;
        switch (name) {
            case "enter":
                enter(ic);
                break;
            case "backspace":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_DEL);
                break;
            case "left":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_DPAD_LEFT);
                break;
            case "right":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_DPAD_RIGHT);
                break;
            case "up":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_DPAD_UP);
                break;
            case "down":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_DPAD_DOWN);
                break;
            case "tab":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_TAB);
                break;
            case "escape":
                sendDownUpKeyEvents(KeyEvent.KEYCODE_ESCAPE);
                break;
            default:
                break;
        }
    }

    // The editor's IME action (Go, Search, Send, Next, Done) where it has one;
    // a newline in multi-line fields; a plain Enter key otherwise.
    private void enter(InputConnection ic) {
        final EditorInfo info = getCurrentInputEditorInfo();
        if (info == null) {
            sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER);
            return;
        }
        final int action = info.imeOptions & EditorInfo.IME_MASK_ACTION;
        final boolean multiLine = (info.inputType & InputType.TYPE_TEXT_FLAG_MULTI_LINE) != 0;
        final boolean noAction = (info.imeOptions & EditorInfo.IME_FLAG_NO_ENTER_ACTION) != 0
                || action == EditorInfo.IME_ACTION_NONE
                || action == EditorInfo.IME_ACTION_UNSPECIFIED;
        if ((info.inputType & InputType.TYPE_MASK_CLASS) == InputType.TYPE_NULL || (noAction && !multiLine))
            sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER);
        else if (multiLine && noAction)
            ic.commitText("\n", 1);
        else
            ic.performEditorAction(action);
    }

    // covered: how much of the app's window the shell's keyboard hides, out of
    // height, both in the shell's logical pixels.
    private void panel(int covered, int height) {
        int px = 0;
        if (covered > 0 && height > 0) {
            final int displayHeight = getSystemService(WindowManager.class)
                    .getMaximumWindowMetrics().getBounds().height();
            px = Math.round((float) Math.min(covered, height) * displayHeight / height);
        }
        coveredPx = px;
        getWindow().getWindow().getDecorView().requestLayout();
        if (!editing)
            return;
        if (px > 0 && !isInputViewShown())
            requestShowSelf(0);
        else if (px == 0 && isInputViewShown())
            requestHideSelf(0);
    }

    private void sendStart(EditorInfo info) {
        final JSONObject m = message("start");
        try {
            m.put("package", info.packageName != null ? info.packageName : "");
            m.put("purpose", contentType.purpose);
            m.put("hints", contentType.hints);
            m.put("inputType", info.inputType);
            m.put("imeOptions", info.imeOptions);
        } catch (JSONException e) {
            return;
        }
        bridge.send(m);
    }

    // Sensitive fields report no text.
    private void sendState() {
        if (!editing)
            return;
        final InputConnection ic = getCurrentInputConnection();
        if (ic == null)
            return;
        String text = "";
        int cursor = 0;
        int anchor = 0;
        if (!contentType.sensitive()) {
            final SurroundingText st = ic.getSurroundingText(SURROUNDING, SURROUNDING, 0);
            if (st != null) {
                text = st.getText().toString();
                cursor = st.getSelectionEnd();
                anchor = st.getSelectionStart();
            }
        }
        final JSONObject m = message("state");
        try {
            m.put("text", text);
            m.put("cursor", cursor);
            m.put("anchor", anchor);
        } catch (JSONException e) {
            return;
        }
        bridge.send(m);
    }

    private static JSONObject message(String type) {
        final JSONObject m = new JSONObject();
        try {
            m.put("t", type);
        } catch (JSONException e) {
            Log.e(Bridge.TAG, "json", e);
        }
        return m;
    }
}
